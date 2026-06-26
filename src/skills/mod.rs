#[allow(unused_imports)]
pub use zeroclaw_runtime::skills::*;

use std::time::Duration;
use zeroclaw_config::skillhub::resolve_skillhub_base_url;
use zeroclaw_runtime::skills::{
    install_skillhub_skill, is_skillhub_source, parse_skillhub_source,
    resolve_skillhub_latest_version,
};

use anyhow::{Context, Result};
use std::path::PathBuf;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};
use zeroclaw_runtime::skills::{
    ScaffoldOptions, SkillFrontmatter, SkillsService, bootstrap_builtin_template_skills,
};
pub mod creator {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::creator::*;
}
pub mod audit {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::audit::*;
}
pub mod skill_tool {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::skill_tool::*;
}
pub mod skill_http {
    #[allow(unused_imports)]
    pub use zeroclaw_runtime::skills::skill_http::*;
}

// The lib target sees this as dead; only the bin target calls it from main.rs.
#[allow(dead_code)]
pub async fn handle_command(
    command: crate::SkillCommands,
    config: &crate::config::Config,
) -> Result<()> {
    let workspace_dir =
        config.agent_workspace_dir(config.resolved_runtime_agent_alias().unwrap_or("default"));
    match command {
        crate::SkillCommands::List => {
            let skills = load_skills_with_config(&workspace_dir, config);
            if skills.is_empty() {
                println!("{}", get_required_cli_string("cli-skills-none-installed"));
                println!();
                println!("{}", get_required_cli_string("cli-skills-create-hint"));
                println!(
                    "              echo '# My Skill' > ~/.zeroclaw/workspace/skills/my-skill/SKILL.md" // i18n-exempt: literal shell command example
                );
                println!();
                println!("{}", get_required_cli_string("cli-skills-install-hint"));
            } else {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-skills-installed-header",
                        &[("count", &skills.len().to_string())],
                    )
                );
                println!();
                for skill in &skills {
                    println!(
                        "  {} {} — {}",
                        console::style(&skill.name).white().bold(),
                        console::style(format!("v{}", skill.version)).dim(),
                        skill.description
                    );
                    if !skill.tools.is_empty() {
                        println!(
                            "    Tools: {}",
                            skill
                                .tools
                                .iter()
                                .map(|t| t.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    if !skill.tags.is_empty() {
                        println!(
                            "    {}",
                            get_required_cli_string_with_args(
                                "cli-skills-tags",
                                &[("tags", &skill.tags.join(", "))],
                            )
                        );
                    }
                }
            }
            println!();
            Ok(())
        }
        crate::SkillCommands::Audit { source } => {
            let source_path = PathBuf::from(&source);
            let target = if source_path.exists() {
                source_path
            } else {
                skills_dir(&workspace_dir).join(zeroclaw_runtime::skills::skill_dir_name(&source))
            };

            if !target.exists() {
                anyhow::bail!("Skill source or installed skill not found: {source}");
            }

            let report = audit::audit_skill_directory_with_options(
                &target,
                audit::SkillAuditOptions {
                    allow_scripts: config.skills.allow_scripts,
                },
            )?;
            if report.is_clean() {
                println!(
                    "  {} Skill audit passed for {} ({} files scanned).",
                    console::style("✓").green().bold(),
                    target.display(),
                    report.files_scanned
                );
                return Ok(());
            }

            println!(
                "  {} Skill audit failed for {}",
                console::style("✗").red().bold(),
                target.display()
            );
            for finding in report.findings {
                println!("    - {finding}");
            }
            anyhow::bail!("Skill audit failed.");
        }
        crate::SkillCommands::Install {
            source,
            no_tier_banner,
        } => {
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-skills-install-start",
                    &[("source", &source)]
                )
            );

            let skills_path = skills_dir(&workspace_dir);
            std::fs::create_dir_all(&skills_path)?;

            // Reuse one reqwest::Client across the auto-fetch + install so we
            // do not pay DNS/TLS handshake twice per CLI invocation.
            let http_client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("zeroclaw-skillhub/0.8")
                .build()
                .context("failed to build reqwest::Client for skill install")?;

            let (installed_dir, files_scanned) = if is_skillhub_source(&source) {
                let (slug, version) = parse_skillhub_source(&source)
                    .with_context(|| format!("invalid SkillHub source: {source}"))?;
                let base_url = resolve_skillhub_base_url(config);
                let version = match version {
                    Some(v) => v,
                    None => {
                        // No @version -> auto-resolve latest from the configured SkillHub.
                        resolve_skillhub_latest_version(&base_url, &slug, &http_client)
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to resolve latest version for '{slug}' from {base_url}"
                                )
                            })?
                    }
                };
                install_skillhub_skill(
                    &base_url,
                    &slug,
                    &version,
                    &skills_path,
                    config.skills.allow_scripts,
                    &http_client,
                    false, // force: CLI uses explicit remove+install
                )
                .await
                .with_context(|| format!("failed to install skill from SkillHub: {source}"))?
            } else if is_git_source(&source) {
                install_git_skill_source(&source, &skills_path, config.skills.allow_scripts)
                    .with_context(|| format!("failed to install git skill source: {source}"))?
            } else if is_registry_source(&source) {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-skills-install-resolving-registry",
                        &[("source", &source)]
                    )
                );
                install_registry_skill_source(
                    &source,
                    &skills_path,
                    config.skills.allow_scripts,
                    &workspace_dir,
                    config.skills.registry_url.as_deref(),
                    no_tier_banner,
                )
                .with_context(|| format!("failed to install skill from registry: {source}"))?
            } else {
                install_local_skill_source(&source, &skills_path, config.skills.allow_scripts)
                    .with_context(|| format!("failed to install local skill source: {source}"))?
            };
            let status = console::style("✓").green().bold().to_string();
            let installed_path = installed_dir.display().to_string();
            let files_scanned = files_scanned.to_string();
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-skills-install-installed-audited",
                    &[
                        ("status", &status),
                        ("path", &installed_path),
                        ("files", &files_scanned)
                    ]
                )
            );

            println!(
                "{}",
                get_required_cli_string("cli-skills-install-security-audit-completed")
            );
            Ok(())
        }
        crate::SkillCommands::BootstrapTemplates => {
            let install_root = config.install_root_dir();
            let mut working = config.clone();
            let summary = bootstrap_builtin_template_skills(&mut working)?;
            let skills_dir = zeroclaw_config::skill_bundles::resolve_directory(
                &working,
                &install_root,
                "default",
            )
            .with_context(|| "failed to resolve default skill bundle directory")?;
            if summary.files_created == 0 {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-skills-bootstrap-existing",
                        &[("dir", &skills_dir.display().to_string())],
                    )
                );
            } else {
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-skills-bootstrap-created",
                        &[
                            ("count", &summary.files_created.to_string()),
                            ("dir", &skills_dir.display().to_string()),
                        ],
                    )
                );
            }
            Ok(())
        }
        crate::SkillCommands::Remove { name } => {
            // Reject path traversal attempts
            if name.contains("..") || name.contains('/') || name.contains('\\') {
                anyhow::bail!("Invalid skill name: {name}");
            }

            // Scan installed skills to find the matching directory.
            // Match by manifest name first, then by normalized slug
            // (the on-disk directory name). The two can differ: slug
            // "taiwan-property-valuation" → dir "taiwan_property_valuation"
            // while the SKILL.toml declares name = "property-valuation".
            let skills = load_skills_with_config(&workspace_dir, config);
            let found = skills.iter().find(|s| s.name == name).or_else(|| {
                let dir_name = zeroclaw_runtime::skills::skill_dir_name(&name);
                skills.iter().find(|s| {
                    s.location
                        .as_ref()
                        .and_then(|loc| loc.parent())
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .is_some_and(|dn| dn == dir_name)
                })
            });
            if let Some(skill) = found {
                let dir = skill
                    .location
                    .as_ref()
                    .and_then(|loc| loc.parent())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| {
                        skills_dir(&workspace_dir)
                            .join(zeroclaw_runtime::skills::skill_dir_name(&name))
                    });
                let removed_name = &skill.name;
                std::fs::remove_dir_all(&dir)?;
                println!(
                    "  {} Skill '{}' removed.",
                    console::style("✓").green().bold(),
                    removed_name
                );
                Ok(())
            } else {
                anyhow::bail!("Skill not found: {name}");
            }
        }
        crate::SkillCommands::Add {
            name,
            bundle,
            description,
            license,
            author,
            version,
            category,
            no_scaffold,
            edit,
        } => handle_add(
            config,
            name,
            bundle,
            description,
            license,
            author,
            version,
            category,
            no_scaffold,
            edit,
        ),
        crate::SkillCommands::Edit { name, bundle, file } => {
            handle_edit(config, name, bundle, file)
        }
        crate::SkillCommands::Bundle { bundle_command } => match bundle_command {
            crate::SkillBundleCommands::List => handle_bundle_list(config),
            crate::SkillBundleCommands::Add { alias, directory } => {
                handle_bundle_add(alias, directory)
            }
            crate::SkillBundleCommands::Remove { alias } => handle_bundle_remove(alias),
            crate::SkillBundleCommands::Show { alias } => handle_bundle_show(config, alias),
        },
        crate::SkillCommands::Test { name, verbose } => {
            let results = if let Some(ref skill_name) = name {
                // Test a single skill
                let source_path = PathBuf::from(skill_name);
                let target = if source_path.exists() {
                    source_path
                } else {
                    skills_dir(&workspace_dir).join(skill_name)
                };

                if !target.exists() {
                    anyhow::bail!("Skill not found: {}", skill_name);
                }

                let r = testing::test_skill(&target, skill_name, verbose)?;
                if r.tests_run == 0 {
                    println!(
                        "  {} No TEST.sh found for skill '{}'.",
                        console::style("-").dim(),
                        skill_name,
                    );
                    return Ok(());
                }
                vec![r]
            } else {
                // Test all skills
                let dirs = vec![skills_dir(&workspace_dir)];
                testing::test_all_skills(&dirs, verbose)?
            };

            testing::print_results(&results);

            let any_failed = results.iter().any(|r| !r.failures.is_empty());
            if any_failed {
                anyhow::bail!("Some skill tests failed.");
            }
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_add(
    config: &crate::config::Config,
    name: String,
    bundle: Option<String>,
    description: Option<String>,
    license: Option<String>,
    author: Option<String>,
    version: Option<String>,
    category: Option<String>,
    no_scaffold: bool,
    edit: bool,
) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let target = service
        .resolve_ref(&name, bundle.as_deref())
        .context("failed to resolve bundle target for skill add")?;

    let description = prompt_for_description(description)?;
    let frontmatter = SkillFrontmatter {
        name: target.name().to_string(),
        description,
        license,
        author,
        version: Some(version.unwrap_or_else(|| "0.1.0".to_string())),
        category,
    };

    let skill_dir = service.scaffold_skill(
        &target,
        frontmatter,
        ScaffoldOptions {
            create_optional_subdirs: !no_scaffold,
            body: String::new(),
        },
    )?;

    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-add-scaffolded",
            &[
                ("target", &target.to_string()),
                ("dir", &skill_dir.display().to_string()),
            ],
        )
    );

    if edit {
        open_in_editor(
            &skill_dir.join(zeroclaw_runtime::skills::constants::SKILL_MANIFEST_FILENAME),
        )?;
    }
    Ok(())
}

fn handle_edit(
    config: &crate::config::Config,
    name: String,
    bundle: Option<String>,
    file: Option<String>,
) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let target = service.resolve_ref(&name, bundle.as_deref())?;

    let summary = service
        .list_skills(Some(target.bundle()))?
        .into_iter()
        .find(|s| s.r#ref.name() == target.name())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"skill_ref": target.to_string()})),
                "skill show: target ref not found"
            );
            anyhow::Error::msg(format!("skill '{target}' not found"))
        })?;

    let path = match file {
        Some(rel) => summary.directory.join(rel),
        None => summary
            .directory
            .join(zeroclaw_runtime::skills::constants::SKILL_MANIFEST_FILENAME),
    };
    if !path.exists() {
        anyhow::bail!("file not found: {}", path.display());
    }
    open_in_editor(&path)
}

fn handle_bundle_add(alias: String, directory: Option<String>) -> Result<()> {
    // Bundle CRUD is config CRUD. Suggest the `zeroclaw config` invocations
    // so the config writer stays single-sourced through api_config /
    // handle_map_key rather than reaching it from here.
    let directory_path = directory.unwrap_or_else(|| format!("shared/skills/{alias}"));
    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-add-prompt",
            &[("alias", &alias), ("dir", &directory_path)],
        )
    );
    Ok(())
}

fn handle_bundle_remove(alias: String) -> Result<()> {
    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-remove-prompt",
            &[("alias", &alias)],
        )
    );
    Ok(())
}

fn print_bundle_include_exclude(include: &[String], exclude: &[String]) {
    if !include.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-include",
                &[("values", &include.join(", "))],
            )
        );
    }
    if !exclude.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-exclude",
                &[("values", &exclude.join(", "))],
            )
        );
    }
}

fn handle_bundle_list(config: &crate::config::Config) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let bundles = service.list_bundles()?;
    if bundles.is_empty() {
        println!(
            "{}",
            zeroclaw_runtime::i18n::get_required_cli_string("cli-skills-bundle-list-empty")
        );
        return Ok(());
    }
    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-list-header",
            &[("count", &bundles.len().to_string())],
        )
    );
    for b in &bundles {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-entry",
                &[
                    ("alias", &b.alias),
                    ("dir", &b.directory.display().to_string()),
                ],
            )
        );
        print_bundle_include_exclude(&b.include, &b.exclude);
    }
    Ok(())
}

fn handle_bundle_show(config: &crate::config::Config, alias: String) -> Result<()> {
    let install_root = config.install_root_dir();
    let service = SkillsService::new(config, install_root);
    let bundles = service.list_bundles()?;
    let bundle = bundles
        .into_iter()
        .find(|b| b.alias == alias)
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"skill_bundle": alias})),
                "skill bundle lookup failed: alias not in config"
            );
            anyhow::Error::msg(format!("skill bundle '{alias}' not configured"))
        })?;

    println!(
        "{}",
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(
            "cli-skills-bundle-entry",
            &[
                ("alias", &bundle.alias),
                ("dir", &bundle.directory.display().to_string()),
            ],
        )
    );
    print_bundle_include_exclude(&bundle.include, &bundle.exclude);

    let skills = service.list_skills(Some(&alias))?;
    if skills.is_empty() {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string("cli-skills-bundle-show-no-skills")
        );
    } else {
        println!(
            "  {}",
            zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                "cli-skills-bundle-show-skills-header",
                &[("count", &skills.len().to_string())],
            )
        );
        for s in &skills {
            println!(
                "    {}",
                zeroclaw_runtime::i18n::get_required_cli_string_with_args(
                    "cli-skills-bundle-show-skill",
                    &[
                        ("name", s.r#ref.name()),
                        ("description", &s.frontmatter.description),
                    ],
                )
            );
        }
    }
    Ok(())
}

fn prompt_for_description(description: Option<String>) -> Result<String> {
    if let Some(d) = description
        && !d.trim().is_empty()
    {
        return Ok(d);
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let prompt: String = dialoguer::Input::new()
            .with_prompt("Skill description (what it does, when to use it)")
            .interact_text()?;
        if prompt.trim().is_empty() {
            anyhow::bail!("description must not be empty");
        }
        Ok(prompt)
    } else {
        anyhow::bail!("--description is required when stdin is not a TTY");
    }
}

fn open_in_editor(path: &std::path::Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor).arg(path).status()?;
    if !status.success() {
        anyhow::bail!("{editor} exited with non-zero status");
    }
    Ok(())
}

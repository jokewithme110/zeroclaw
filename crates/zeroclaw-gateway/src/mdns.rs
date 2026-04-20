//! Gateway mDNS advertisement (local network discovery).

use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use zeroclaw_config::schema::GatewayMdnsConfig;

pub struct MdnsAdvertiser {
    _daemon: ServiceDaemon,
    _service_fullname: String,
}

fn default_instance_name() -> String {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "zeroclaw".into());
    format!("{host}-zeroclaw")
}

fn default_mdns_hostname() -> String {
    let base = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zeroclaw-gateway".into());
    let label = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let normalized = if label.is_empty() { "zeroclaw-gateway" } else { label.as_str() };
    format!("{normalized}.local.")
}

fn best_effort_local_ip_txt() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(ip) = local_ip_address::local_ip() {
        match ip {
            IpAddr::V4(v4) => out.push(("ip".into(), v4.to_string())),
            IpAddr::V6(v6) => out.push(("ip6".into(), v6.to_string())),
        }
    }
    if let Ok(ip6) = local_ip_address::local_ipv6() {
        out.push(("ip6".into(), ip6.to_string()));
    }
    out
}

fn best_effort_advertise_ip() -> Option<IpAddr> {
    match local_ip_address::local_ip() {
        Ok(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

pub fn start_gateway_mdns(
    cfg: &GatewayMdnsConfig,
    port: u16,
    path_prefix: &str,
) -> anyhow::Result<Option<MdnsAdvertiser>> {
    if !cfg.enabled {
        return Ok(None);
    }
    let service_type = cfg.service_type.trim();
    if service_type.is_empty() {
        anyhow::bail!("gateway.mdns.service_type must not be empty");
    }
    let instance = cfg
        .instance_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(default_instance_name);

    let mut properties: HashMap<String, String> = HashMap::new();
    properties.insert("v".into(), "1".into());
    properties.insert("port".into(), port.to_string());
    if cfg.include_path_prefix && !path_prefix.is_empty() {
        properties.insert("path_prefix".into(), path_prefix.to_owned());
    }
    if cfg.include_ws_path {
        let ws_path = if path_prefix.is_empty() { "/".to_owned() } else { format!("{path_prefix}/") };
        properties.insert("ws".into(), ws_path);
    }
    if cfg.include_local_ip_txt {
        for (k, v) in best_effort_local_ip_txt() {
            properties.entry(k).or_insert(v);
        }
    }

    let daemon = ServiceDaemon::new().context("create mDNS daemon")?;
    let host_name = default_mdns_hostname();
    let service = if let Some(ip) = best_effort_advertise_ip() {
        ServiceInfo::new(service_type, &instance, &host_name, ip, port, properties)
    } else {
        ServiceInfo::new(service_type, &instance, &host_name, (), port, properties)
    }
    .context("build mDNS service info")?;

    let fullname = service.get_fullname().to_string();
    daemon.register(service).context("register mDNS service")?;
    Ok(Some(MdnsAdvertiser {
        _daemon: daemon,
        _service_fullname: fullname,
    }))
}

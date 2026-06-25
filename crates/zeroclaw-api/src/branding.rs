//! 品牌配置 - 运行时从环境变量读取
//!
//! 所有 zeroclaw-* crates 都应引用此模块获取品牌配置，
//! 以保证单一数据源原则。
//!
//! 环境变量：
//! - BRAND: 展示名（大写），用于系统提示词/标题/组织名，默认 "ZeroClaw"
//! - BRAND_SLUG: slug名（小写+连字符），用于目录/环境变量前缀，默认 "zeroclaw"
//!
//! 派生规则：
//! - product_name / org_name / web_title -> BRAND
//! - data_dir -> ".{BRAND_SLUG}"
//! - env_prefix -> BRAND_SLUG 的大写

/// 获取展示名（大写）
fn brand_raw() -> String {
    std::env::var("BRAND").unwrap_or_else(|_| "ZeroClaw".into())
}

/// 获取 slug 名（小写+连字符）
fn brand_slug_raw() -> String {
    std::env::var("BRAND_SLUG").unwrap_or_else(|_| "zeroclaw".into())
}

/// 产品名称（用于显示），即展示名
pub fn product_name() -> String {
    brand_raw()
}

/// 数据目录名前缀（不带点），格式为 ".{brand_slug}"
pub fn data_dir() -> String {
    brand_slug_raw()
}

pub fn bin_name() -> String {
    std::env::var("BRAND_BIN_NAME").unwrap_or_else(|_| product_name())
}

/// 环境变量前缀（大写），即 slug 的大写
pub fn env_prefix() -> String {
    brand_slug_raw().to_uppercase()
}

/// 在用户可见文本里把品牌字面量替换为运行时的 BRAND / BRAND_SLUG。
///
/// 调用点仅限最终给人类读的输出（日志消息、CLI banner 等）。
/// **不可**用于 crate 路径、tracing target、协议标识符、localStorage 键
/// 等技术标识符 — 替换这些会破坏编译、日志路由或客户端兼容性。
///
/// 默认 BRAND="ZeroClaw"、BRAND_SLUG="zeroclaw" 时为 no-op；
/// 当环境变量被设置为非默认值时才真正替换。
///
/// 替换三类字面量：
/// - `ZeroClaw`（显示名） → `BRAND` / `product_name()`
/// - `zeroclaw`（slug）    → `BRAND_SLUG`
/// - `ZEROCLAW`（env 前缀） → `env_prefix()`（BRAND_SLUG 大写）
pub fn rewrite(text: &str) -> std::borrow::Cow<'_, str> {
    let display = product_name();
    let slug = brand_slug_raw();
    let prefix = env_prefix();
    let display_changed = display != "ZeroClaw" && text.contains("ZeroClaw");
    let slug_changed = slug != "zeroclaw" && text.contains("zeroclaw");
    let prefix_changed = prefix != "ZEROCLAW" && text.contains("ZEROCLAW");
    if !display_changed && !slug_changed && !prefix_changed {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(text);
    if prefix_changed {
        out = out.replace("ZEROCLAW", &prefix);
    }
    if display_changed {
        out = out.replace("ZeroClaw", &display);
    }
    if slug_changed {
        out = out.replace("zeroclaw", &slug);
    }
    std::borrow::Cow::Owned(out)
}

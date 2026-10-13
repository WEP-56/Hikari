use hikari_agent::{AgentError, Result};
use std::time::Duration;

#[derive(Debug)]
pub struct ReleaseInfo {
    pub tag: String,
    pub url: String,
    pub newer: bool,
}
pub fn check_release(current: &str) -> Result<ReleaseInfo> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
        .get("https://api.github.com/repos/WEP-56/Hikari/releases/latest")
        .set("User-Agent", "Hikari-update-check")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => AgentError::new("仓库尚未发布正式 Release"),
            ureq::Error::Status(403 | 429, _) => AgentError::new("GitHub 请求受限，请稍后重试"),
            _ => AgentError::new("无法连接 GitHub，请检查网络"),
        })?;
    let value: serde_json::Value = response
        .into_json()
        .map_err(|_| AgentError::new("GitHub 响应格式错误"))?;
    release_info(&value, current)
}
pub fn release_info(value: &serde_json::Value, current: &str) -> Result<ReleaseInfo> {
    let tag = value["tag_name"]
        .as_str()
        .ok_or_else(|| AgentError::new("Release 缺少 tag"))?;
    let url = value["html_url"]
        .as_str()
        .filter(|url| url.starts_with("https://github.com/WEP-56/Hikari/releases/"))
        .ok_or_else(|| AgentError::new("Release 地址无效"))?;
    let latest = semver::Version::parse(tag.trim_start_matches('v'))
        .map_err(|_| AgentError::new(format!("无法比较 Release tag：{tag}；可在发布版本页查看")))?;
    let current =
        semver::Version::parse(current).map_err(|_| AgentError::new("当前版本格式错误"))?;
    Ok(ReleaseInfo {
        tag: tag.into(),
        url: url.into(),
        newer: latest > current,
    })
}

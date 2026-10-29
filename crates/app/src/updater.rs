//! Self-update from GitHub Releases: replaces the AppImage in place, or installs the new .deb.

use std::path::PathBuf;

const REPO: &str = "genetzhs/sublimers";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How this copy of the app was installed, which decides how it updates.
#[derive(Clone, Debug)]
pub enum Install {
    AppImage(PathBuf),
    Deb,
    Rpm,
    /// Built from source or unpacked by hand: only point at the release page.
    Other,
}

pub fn install_kind() -> Install {
    if let Some(p) = std::env::var_os("APPIMAGE") {
        return Install::AppImage(PathBuf::from(p));
    }
    let exe = std::env::current_exe().unwrap_or_default();
    if exe.starts_with("/usr/bin") {
        if std::path::Path::new("/var/lib/dpkg/info/sublimers.list").exists() {
            return Install::Deb;
        }
        let rpm_owned = std::process::Command::new("rpm").args(["-q", "sublimers"]).output().is_ok_and(|o| o.status.success());
        if rpm_owned {
            return Install::Rpm;
        }
    }
    Install::Other
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub notes: String,
    pub appimage_url: Option<String>,
    pub deb_url: Option<String>,
    pub rpm_url: Option<String>,
}

fn version_parts(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    version_parts(latest) > version_parts(current)
}

pub fn fetch_latest() -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = ureq::get(&url)
        .set("User-Agent", concat!("sublimers/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => "No releases published yet.".to_string(),
            e => format!("Could not reach GitHub: {e}"),
        })?;
    let v: serde_json::Value = serde_json::from_reader(resp.into_reader()).map_err(|e| e.to_string())?;
    let asset = |suffix: &str| {
        v["assets"].as_array()?.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with(suffix)))?["browser_download_url"].as_str().map(String::from)
    };
    Ok(Release {
        version: v["tag_name"].as_str().unwrap_or_default().trim_start_matches('v').to_string(),
        page: v["html_url"].as_str().unwrap_or_default().to_string(),
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        appimage_url: asset("x86_64.AppImage"),
        deb_url: asset("_amd64.deb"),
        rpm_url: asset(".x86_64.rpm"),
    })
}

fn download(url: &str, to: &std::path::Path) -> Result<(), String> {
    let resp = ureq::get(url).set("User-Agent", "sublimers").call().map_err(|e| format!("Download failed: {e}"))?;
    let mut f = std::fs::File::create(to).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp.into_reader(), &mut f).map_err(|e| format!("Download failed: {e}"))?;
    Ok(())
}

/// Download and install `rel`. Returns a message for the user.
pub fn apply(rel: &Release, kind: &Install) -> Result<String, String> {
    match kind {
        Install::AppImage(path) => {
            let url = rel.appimage_url.as_deref().ok_or("This release has no AppImage.")?;
            // Download next to the AppImage, then swap it in atomically.
            let tmp = path.with_extension("download");
            download(url, &tmp)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, path).map_err(|e| format!("Could not replace {}: {e}", path.display()))?;
            Ok(format!("Updated to {}. Restart Sublimers to use it.", rel.version))
        }
        Install::Deb | Install::Rpm => {
            let deb = matches!(kind, Install::Deb);
            let url = if deb { rel.deb_url.as_deref() } else { rel.rpm_url.as_deref() };
            let url = url.ok_or("This release has no package for this system.")?;
            let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("sublimers");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let file = dir.join(if deb { format!("sublimers_{}_amd64.deb", rel.version) } else { format!("sublimers-{}-1.x86_64.rpm", rel.version) });
            download(url, &file)?;
            // pkexec shows the system's own password dialog; the package manager installs the file.
            let has = |c: &str| std::process::Command::new("sh").args(["-c", &format!("command -v {c}")]).output().is_ok_and(|o| o.status.success());
            let args: Vec<&str> = if deb {
                vec!["apt-get", "install", "-y", "--allow-downgrades"]
            } else if has("dnf") {
                vec!["dnf", "install", "-y"]
            } else if has("zypper") {
                vec!["zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]
            } else {
                vec!["rpm", "-U"]
            };
            let ok = std::process::Command::new("pkexec")
                .args(args)
                .arg(&file)
                .status()
                .map_err(|e| format!("Could not run pkexec: {e}"))?
                .success();
            if ok {
                Ok(format!("Installed {}. Restart Sublimers to use it.", rel.version))
            } else {
                Err(format!("Installation was cancelled or failed. The package is at {}", file.display()))
            }
        }
        Install::Other => Err(format!("Download it from {}", rel.page)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
        assert!(is_newer("0.1.10", "0.1.9"));
    }
}

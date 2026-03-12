use std::process::Command;

const GITHUB_API_URL: &str = "https://api.github.com/repos/wiggzz/claude-queue/releases/latest";

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let current = env!("CARGO_PKG_VERSION");

    // Fetch latest release info from GitHub
    let output = Command::new("curl")
        .args([
            "-sS",
            "-H",
            "Accept: application/vnd.github+json",
            GITHUB_API_URL,
        ])
        .output()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to fetch latest release: {stderr}").into());
    }

    let body: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))?;

    let tag = body["tag_name"]
        .as_str()
        .ok_or("No tag_name in release response")?;

    let latest = tag.strip_prefix('v').unwrap_or(tag);

    if latest == current {
        println!("Already up to date (v{current})");
        return Ok(());
    }

    // Detect OS and architecture
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target_os = match os {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        _ => return Err(format!("Unsupported OS: {os}").into()),
    };

    let target_arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return Err(format!("Unsupported architecture: {arch}").into()),
    };

    let asset_name = format!("cq-{tag}-{target_arch}-{target_os}.tar.gz");

    // Find the download URL from release assets
    let assets = body["assets"]
        .as_array()
        .ok_or("No assets in release response")?;

    let download_url = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| format!("No asset found matching {asset_name}"))?;

    // Create a temp directory for staging
    let tmpdir = std::env::temp_dir().join(format!("cq-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir)?;

    let tarball_path = tmpdir.join(&asset_name);

    // Download the tarball
    eprintln!("Downloading {asset_name}...");
    let dl = Command::new("curl")
        .args(["-sSL", "-o"])
        .arg(&tarball_path)
        .arg(download_url)
        .status()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if !dl.success() {
        let _ = std::fs::remove_dir_all(&tmpdir);
        return Err("Failed to download release asset".into());
    }

    // Extract the tarball
    let extract = Command::new("tar")
        .args(["-xzf"])
        .arg(&tarball_path)
        .arg("-C")
        .arg(&tmpdir)
        .status()
        .map_err(|e| format!("Failed to run tar: {e}"))?;

    if !extract.success() {
        let _ = std::fs::remove_dir_all(&tmpdir);
        return Err("Failed to extract release archive".into());
    }

    // Replace the current binary
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("Failed to determine current executable path: {e}"))?;

    let new_binary = tmpdir.join("cq");
    if !new_binary.exists() {
        let _ = std::fs::remove_dir_all(&tmpdir);
        return Err("Extracted archive does not contain 'cq' binary".into());
    }

    // On Unix, we can atomically replace by renaming the old binary and moving the new one in
    // First remove the old binary (or rename), then copy new one
    let backup = current_exe.with_extension("old");
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }

    // Rename current binary to .old backup
    std::fs::rename(&current_exe, &backup)
        .map_err(|e| format!("Failed to move current binary: {e}. Try running with sudo."))?;

    // Move new binary into place
    if let Err(e) = std::fs::rename(&new_binary, &current_exe) {
        // Try to restore the backup
        let _ = std::fs::rename(&backup, &current_exe);
        let _ = std::fs::remove_dir_all(&tmpdir);
        return Err(format!("Failed to install new binary: {e}. Try running with sudo.").into());
    }

    // Set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755));
    }

    // Clean up
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::remove_dir_all(&tmpdir);

    println!("Updated to v{latest}");
    Ok(())
}

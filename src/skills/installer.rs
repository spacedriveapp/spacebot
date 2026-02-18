//! Skill installation from skills.sh registry and GitHub repos.
//!
//! Supports installing skills from:
//! - GitHub repos: `owner/repo` or `owner/repo/skill-name`
//! - .skill files (zip archives with .skill extension)
//! - Direct URLs to skill archives

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Install a skill from a GitHub repository.
///
/// Format: `owner/repo` or `owner/repo/skill-name`
///
/// Downloads the repo as a zip, extracts the skill directory, and installs
/// to the target directory.
pub async fn install_from_github(
    spec: &str,
    target_dir: &Path,
) -> Result<Vec<String>> {
    let (owner, repo, skill_path) = parse_github_spec(spec)?;
    
    // Download the repo as a zip from GitHub
    let download_url = format!(
        "https://github.com/{}/{}/archive/refs/heads/main.zip",
        owner, repo
    );
    
    tracing::info!(
        owner = %owner,
        repo = %repo,
        skill_path = ?skill_path,
        url = %download_url,
        "downloading skill from GitHub"
    );
    
    let client = reqwest::Client::new();
    let response = client
        .get(&download_url)
        .send()
        .await
        .context("failed to download GitHub archive")?;
    
    if !response.status().is_success() {
        anyhow::bail!(
            "failed to download from GitHub: HTTP {}",
            response.status()
        );
    }
    
    let bytes = response
        .bytes()
        .await
        .context("failed to read response body")?;
    
    // Write to temp file
    let temp_dir = tempfile::tempdir().context("failed to create temp dir")?;
    let zip_path = temp_dir.path().join("skill.zip");
    
    let mut file = fs::File::create(&zip_path)
        .await
        .context("failed to create temp file")?;
    file.write_all(&bytes)
        .await
        .context("failed to write zip file")?;
    file.sync_all().await?;
    drop(file);
    
    // Extract and install
    let installed = extract_and_install(&zip_path, target_dir, skill_path.as_deref()).await?;
    
    tracing::info!(
        installed = ?installed,
        "skills installed from GitHub"
    );
    
    Ok(installed)
}

/// Install a skill from a .skill file (zip archive).
pub async fn install_from_file(
    skill_file: &Path,
    target_dir: &Path,
) -> Result<Vec<String>> {
    if !skill_file.exists() {
        anyhow::bail!("skill file does not exist: {}", skill_file.display());
    }
    
    tracing::info!(
        file = %skill_file.display(),
        "installing skill from file"
    );
    
    let installed = extract_and_install(skill_file, target_dir, None).await?;
    
    tracing::info!(
        installed = ?installed,
        "skills installed from file"
    );
    
    Ok(installed)
}

/// Extract a zip archive and install skills to the target directory.
///
/// If `skill_path` is provided, only install that specific skill subdirectory.
/// Otherwise, scan for all SKILL.md files in the archive.
async fn extract_and_install(
    zip_path: &Path,
    target_dir: &Path,
    skill_path: Option<&str>,
) -> Result<Vec<String>> {
    let file = std::fs::File::open(zip_path)
        .context("failed to open zip file")?;
    
    let mut archive = zip::ZipArchive::new(file)
        .context("failed to read zip archive")?;
    
    let temp_extract = tempfile::tempdir()
        .context("failed to create temp extract dir")?;
    
    // Extract entire archive to temp
    archive.extract(temp_extract.path())
        .context("failed to extract archive")?;
    
    // Find the root directory (GitHub zips have a single root dir like "repo-main/")
    let root = find_archive_root(temp_extract.path()).await?;
    
    // Find skills to install
    let skills_to_install = if let Some(path) = skill_path {
        // Install specific skill - check direct path first, then search recursively.
        // Repos often nest skills in subdirectories (e.g. anthropics/skills has
        // skills under a `skills/` subdirectory, so `frontend-design` lives at
        // `skills-main/skills/frontend-design/SKILL.md`).
        let direct = root.join(path);
        if direct.join("SKILL.md").exists() {
            vec![direct]
        } else {
            let all = find_skills(&root).await?;
            let matching: Vec<_> = all
                .into_iter()
                .filter(|d| {
                    d.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n == path)
                })
                .collect();
            if matching.is_empty() {
                anyhow::bail!(
                    "skill not found: {} (no SKILL.md in any matching directory)",
                    path
                );
            }
            matching
        }
    } else {
        // Find all SKILL.md files
        find_skills(&root).await?
    };
    
    if skills_to_install.is_empty() {
        anyhow::bail!("no skills found in archive");
    }
    
    // Copy each skill to target directory
    let mut installed = Vec::new();
    
    for skill_dir in skills_to_install {
        let skill_name = skill_dir
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid skill directory name")?;
        
        let target_skill_dir = target_dir.join(skill_name);
        
        // Remove existing skill if present
        if target_skill_dir.exists() {
            tracing::warn!(
                skill = %skill_name,
                "removing existing skill"
            );
            fs::remove_dir_all(&target_skill_dir).await?;
        }
        
        // Copy skill directory
        copy_dir_recursive(&skill_dir, &target_skill_dir).await?;
        
        installed.push(skill_name.to_string());
        
        tracing::debug!(
            skill = %skill_name,
            path = %target_skill_dir.display(),
            "skill installed"
        );
    }
    
    Ok(installed)
}

/// Parse a GitHub spec: `owner/repo` or `owner/repo/skill-name`
fn parse_github_spec(spec: &str) -> Result<(String, String, Option<String>)> {
    let parts: Vec<&str> = spec.split('/').collect();
    
    match parts.len() {
        2 => {
            // owner/repo
            Ok((parts[0].to_string(), parts[1].to_string(), None))
        }
        3 => {
            // owner/repo/skill-name
            Ok((
                parts[0].to_string(),
                parts[1].to_string(),
                Some(parts[2].to_string()),
            ))
        }
        _ => anyhow::bail!(
            "invalid GitHub spec (expected owner/repo or owner/repo/skill-name): {}",
            spec
        ),
    }
}

/// Find the root directory in an extracted archive.
///
/// GitHub zips have a single root directory like "repo-main/".
async fn find_archive_root(extract_dir: &Path) -> Result<PathBuf> {
    let mut entries = fs::read_dir(extract_dir).await?;
    
    let mut root = None;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            if root.is_some() {
                // Multiple top-level directories, use extract dir itself
                return Ok(extract_dir.to_path_buf());
            }
            root = Some(path);
        }
    }
    
    Ok(root.unwrap_or_else(|| extract_dir.to_path_buf()))
}

/// Find all directories containing SKILL.md files.
async fn find_skills(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut skills = Vec::new();
    
    let mut queue = vec![dir.to_path_buf()];
    
    while let Some(current) = queue.pop() {
        let mut entries = fs::read_dir(&current).await?;
        
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            
            if path.is_dir() {
                // Check if this dir has SKILL.md
                if path.join("SKILL.md").exists() {
                    skills.push(path.clone());
                } else {
                    // Recurse into subdirectories
                    queue.push(path);
                }
            }
        }
    }
    
    Ok(skills)
}

/// Recursively copy a directory.
fn copy_dir_recursive<'a>(
    src: &'a Path,
    dst: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        fs::create_dir_all(dst).await?;
        
        let mut entries = fs::read_dir(src).await?;
        
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = src_path
                .file_name()
                .context("invalid file name")?;
            let dst_path = dst.join(file_name);
            
            if src_path.is_dir() {
                copy_dir_recursive(&src_path, &dst_path).await?;
            } else {
                fs::copy(&src_path, &dst_path).await?;
            }
        }
        
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_github_spec() {
        let (owner, repo, skill) = parse_github_spec("vercel-labs/agent-skills").unwrap();
        assert_eq!(owner, "vercel-labs");
        assert_eq!(repo, "agent-skills");
        assert_eq!(skill, None);
        
        let (owner, repo, skill) = parse_github_spec("anthropics/skills/pdf").unwrap();
        assert_eq!(owner, "anthropics");
        assert_eq!(repo, "skills");
        assert_eq!(skill, Some("pdf".to_string()));
    }
    
    #[test]
    fn test_parse_github_spec_invalid() {
        assert!(parse_github_spec("invalid").is_err());
        assert!(parse_github_spec("too/many/slashes/here").is_err());
    }
}

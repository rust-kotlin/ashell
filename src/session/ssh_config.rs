use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::BaseDirs;

/// A parsed entry from ~/.ssh/config
#[derive(Debug, Clone)]
pub struct SshConfigEntry {
    /// The Host pattern (alias) from the config, e.g. "myserver"
    pub host_alias: String,
    /// The actual hostname (HostName), defaults to the host alias if not specified
    pub hostname: String,
    /// The user, defaults to empty (will use current OS user)
    pub user: String,
    /// The port, defaults to 22
    pub port: u16,
    /// Identity files specified for this host
    pub identity_files: Vec<String>,
    /// Whether this is a wildcard/pattern host (Host * or Host *)
    pub is_wildcard: bool,
}

/// Parse ~/.ssh/config and return a list of concrete host entries.
/// Wildcard patterns (Host *) are excluded.
pub fn parse_ssh_config() -> Result<Vec<SshConfigEntry>> {
    let config_path = ssh_config_path()?;
    if !config_path.is_file() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    parse_ssh_config_content(&content)
}

fn ssh_config_path() -> Result<PathBuf> {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".ssh/config"))
        .context("failed to determine home directory")
}

/// Parse the content of an ssh config file into entries.
pub fn parse_ssh_config_content(content: &str) -> Result<Vec<SshConfigEntry>> {
    let mut entries: Vec<SshConfigEntry> = Vec::new();
    let mut current_host: Option<SshConfigEntry> = None;

    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split into keyword and value
        // OpenSSH supports both "keyword value" and "keyword=value" formats
        let (keyword, value) = if let Some(pos) = line.find('=') {
            (&line[..pos], line[pos + 1..].trim())
        } else if let Some(pos) = line.find(' ') {
            (&line[..pos], line[pos..].trim())
        } else {
            continue;
        };

        let keyword_lower = keyword.trim().to_lowercase();
        let value = value.trim();

        match keyword_lower.as_str() {
            "host" => {
                // Save previous entry if it exists and is not a wildcard
                if let Some(entry) = current_host.take() {
                    if !entry.is_wildcard {
                        entries.push(entry);
                    }
                }

                // Host line may contain multiple patterns (Host a b c)
                // Take the first non-wildcard pattern as the display alias
                let patterns: Vec<&str> = value.split_whitespace().collect();
                if patterns.is_empty() {
                    continue;
                }
                let host_alias = patterns
                    .iter()
                    .find(|p| !p.contains('*') && !p.contains('?'))
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| patterns[0].to_string());

                let is_wildcard = value.contains('*') || value.contains('?');

                current_host = Some(SshConfigEntry {
                    host_alias: host_alias.clone(),
                    hostname: host_alias,
                    user: String::new(),
                    port: 22,
                    identity_files: Vec::new(),
                    is_wildcard,
                });
            }
            "hostname" => {
                if let Some(entry) = current_host.as_mut() {
                    entry.hostname = value.to_string();
                }
            }
            "user" => {
                if let Some(entry) = current_host.as_mut() {
                    entry.user = value.to_string();
                }
            }
            "port" => {
                if let Some(entry) = current_host.as_mut() {
                    entry.port = value.parse::<u16>().unwrap_or(22);
                }
            }
            "identityfile" => {
                if let Some(entry) = current_host.as_mut() {
                    entry.identity_files.push(value.to_string());
                }
            }
            // Skip Match blocks and Include directives — not supported yet
            "match" | "include" => {
                // If we encounter a Match block, flush the current Host entry
                // since Match blocks apply to all subsequent hosts until the next Match/Host
                if let Some(entry) = current_host.take() {
                    if !entry.is_wildcard {
                        entries.push(entry);
                    }
                }
                // Don't create a new entry for Match/Include
            }
            _ => {}
        }
    }

    // Save the last entry if it's not a wildcard
    if let Some(entry) = current_host.take() {
        if !entry.is_wildcard {
            entries.push(entry);
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ssh_config_content() {
        let content = r#"
            Host myhost
                HostName 1.2.3.4
                User git
                Port 2222
                IdentityFile ~/.ssh/id_rsa

            Host
            Host = 

            Host anotherhost
                HostName 5.6.7.8
        "#;

        let entries = parse_ssh_config_content(content).unwrap();
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].host_alias, "myhost");
        assert_eq!(entries[0].hostname, "1.2.3.4");
        assert_eq!(entries[0].user, "git");
        assert_eq!(entries[0].port, 2222);
        assert_eq!(entries[0].identity_files, vec!["~/.ssh/id_rsa".to_string()]);

        assert_eq!(entries[1].host_alias, "anotherhost");
        assert_eq!(entries[1].hostname, "5.6.7.8");
    }
}

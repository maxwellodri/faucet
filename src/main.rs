use anyhow::Result;
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::{stdin, IsTerminal, Read, Write};
use tracing::{debug, error, trace};
use itertools::Either;

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Scorer {
    Regex {
        regex: String,
        command_label: String,
        score_change: i32,
    },
    Command {
        command: String,
        command_label: String,
        score_change: i32,
    },
    RegexMulti {
        regex: String,
        scores: Vec<(String, i32)>,
    },
    CommandMulti {
        command: String,
        scores: Vec<(String, i32)>,
    },
}

impl Scorer {
    fn command_labels(&self) -> impl Iterator<Item = &str> {
        match self {
            Scorer::Regex { command_label, .. } | Scorer::Command { command_label, .. } => {
                Either::Left(std::iter::once(command_label.as_str()))
            }
            Scorer::RegexMulti { scores, .. } | Scorer::CommandMulti { scores, .. } => {
                Either::Right(scores.iter().map(|(label, _)| label.as_str()))
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Command {
    display: String,
    command: String,
}

fn default_min_threshold() -> i32 {
    10
}

fn default_max_threshold() -> i32 {
    100
}

#[derive(Serialize, Deserialize)]
struct Config {
    commands: IndexMap<String, Command>,
    scorers: Vec<Scorer>,
    #[serde(default = "default_min_threshold")]
    auto_select_min_threshold: i32,
    #[serde(default = "default_max_threshold")]
    auto_select_max_threshold: i32,
}

fn check_command_exists(command: &str) -> Result<()> {
    let status = std::process::Command::new("which").arg(command).status()?;

    if !status.success() {
        anyhow::bail!("Required command '{}' not found in PATH", command);
    }
    Ok(())
}

fn validate_environment(config: &Config) -> Result<()> {
    for cmd in ["file", "dmenu", "xclip", "sh"] {
        check_command_exists(cmd)?;
    }

    if config.auto_select_min_threshold >= config.auto_select_max_threshold {
        anyhow::bail!(
            "Bad auto select values: min ({}) >= max ({})",
            config.auto_select_min_threshold,
            config.auto_select_max_threshold
        );
    }

let missing_commands: Vec<(String, String, String)> = config
    .scorers
    .iter()
    .flat_map(|scorer| {
        scorer.command_labels().filter_map(move |label| {
            if !config.commands.contains_key(label) {
                Some(match scorer {
                    Scorer::Regex { regex, .. } => {
                        ("regex".to_string(), regex.clone(), label.to_string())
                    }
                    Scorer::Command { command, .. } => {
                        ("command".to_string(), command.clone(), label.to_string())
                    }
                    Scorer::RegexMulti { regex, .. } => {
                        ("regex_multi".to_string(), regex.clone(), label.to_string())
                    }
                    Scorer::CommandMulti { command, .. } => {
                        ("command_multi".to_string(), command.clone(), label.to_string())
                    }
                })
            } else {
                None
            }
        })
    })
    .collect();

    if !missing_commands.is_empty() {
        let error_msg = missing_commands
            .iter()
            .map(|(kind, data, command)| format!("{} '{}' -> command '{}'", kind, data, command))
            .collect::<Vec<_>>()
            .join(", ");

        anyhow::bail!("Scorers reference non-existent commands: {}", error_msg);
    }
    Ok(())
}

enum Data {
    Text(String),
    Binary(Vec<u8>),
}

impl Data {
    fn get_text_for_matching(&self, temp_file_path: &str) -> Result<String> {
        match self {
            Data::Text(s) => Ok(s.trim_end().to_string()),
            Data::Binary(_) => {
                let output = std::process::Command::new("file")
                    .args(["--mime-type", "-b", temp_file_path])
                    .output()?;
                Ok(String::from_utf8(output.stdout)?.trim().to_string())
            }
        }
    }

    fn write_to_temp_file(&self, path: &str) -> Result<()> {
        match self {
            Data::Text(s) => std::fs::write(path, s.as_bytes())?,
            Data::Binary(bytes) => std::fs::write(path, bytes)?,
        }
        Ok(())
    }

    fn is_text(&self) -> bool {
        matches!(self, Data::Text(..))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .init();
    let config_path = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?.join("faucet").join("faucet.yaml");
    
    let config_content = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read config file at '{}': {}", config_path.display(), e))?;
    
    let config: Config = serde_yaml::from_str(&config_content)
        .map_err(|e| anyhow::anyhow!(
            "Failed to parse config file '{}':\n{}",
            config_path.display(),
            e
        ))?;
    
    validate_environment(&config)?;

    debug!(
        "Loaded {} commands and {} scorers",
        config.commands.len(),
        config.scorers.len()
    );
    let args: Vec<String> = std::env::args().collect();
    let data_source: &str;
    let data: Data = match args.len() {
        1 => {
            if !stdin().is_terminal() {
                let mut buffer = Vec::new();
                match stdin().read_to_end(&mut buffer) {
                    Ok(_) if !buffer.is_empty() => {
                        data_source = "stdin";
                        if let Ok(text) = String::from_utf8(buffer.clone()) {
                            Data::Text(text)
                        } else {
                            Data::Binary(buffer)
                        }
                    }
                    _ => {
                        data_source = "clipboard";
                        let clipboard_bytes = std::process::Command::new("sh")
                            .args(["-c", "xclip -selection clipboard -o"])
                            .output()?
                            .stdout;

                        if let Ok(text) = String::from_utf8(clipboard_bytes.clone()) {
                            Data::Text(text)
                        } else {
                            Data::Binary(clipboard_bytes)
                        }
                    }
                }
            } else {
                data_source = "clipboard";
                let clipboard_bytes = std::process::Command::new("sh")
                    .args(["-c", "xclip -selection clipboard -o"])
                    .output()?
                    .stdout;

                if let Ok(text) = String::from_utf8(clipboard_bytes.clone()) {
                    Data::Text(text)
                } else {
                    Data::Binary(clipboard_bytes)
                }
            }
        }
        2 if args[1] == "sel" => {
            data_source = "selection";

            let targets = std::process::Command::new("sh")
                .args(["-c", "xclip -selection primary -t TARGETS -o"])
                .output()?
                .stdout;

            let targets_str = String::from_utf8_lossy(&targets);

            if targets_str.contains("image/") {
                let selection_bytes = if targets_str.contains("image/png") {
                    std::process::Command::new("sh")
                        .args(["-c", "xclip -selection primary -t image/png -o"])
                        .output()?
                        .stdout
                } else if targets_str.contains("image/jpeg") {
                    std::process::Command::new("sh")
                        .args(["-c", "xclip -selection primary -t image/jpeg -o"])
                        .output()?
                        .stdout
                } else {
                    std::process::Command::new("sh")
                        .args(["-c", "xclip -selection primary -t image -o"])
                        .output()?
                        .stdout
                };
                Data::Binary(selection_bytes)
            } else {
                let selection_bytes = std::process::Command::new("sh")
                    .args(["-c", "xclip -selection primary -o"])
                    .output()?
                    .stdout;

                if let Ok(text) = String::from_utf8(selection_bytes.clone()) {
                    Data::Text(text)
                } else {
                    Data::Binary(selection_bytes)
                }
            }
        }
        3 if args[1] == "file" => {
            data_source = "file";
            let file_path = &args[2];
            let file_bytes = std::fs::read(file_path)?;

            if let Ok(text) = String::from_utf8(file_bytes.clone()) {
                Data::Text(text)
            } else {
                Data::Binary(file_bytes)
            }
        }
        _ => {
            data_source = "command line";
            Data::Text(args[1..].join(" "))
        }
    };

    let temp_file = "/tmp/faucet_data";
    data.write_to_temp_file(temp_file)?;

    let text_for_matching = data.get_text_for_matching(temp_file)?;
    let (data_kind, data_as_text) = match data {
        Data::Text(ref text) => ("Text", text.clone()),
        Data::Binary(..) => ("Data", format!("[Binary: {}]", text_for_matching)),
    };

    debug!(
        "text_for_matching: {}",
        text_for_matching.chars().take(100).collect::<String>()
    );
    debug!("{data_kind} from {data_source} to be plumbed: '{data_as_text}'");

    let mut scored_commands: IndexMap<String, (Command, i32)> = config
        .commands
        .iter()
        .map(|(label, cmd)| (label.clone(), (cmd.clone(), 0)))
        .collect();

    config.scorers.iter().for_each(|scorer| match scorer {
    Scorer::Regex {
        regex,
        command_label,
        score_change,
    } => {
        if let Ok(re) = Regex::new(regex)
            && re.is_match(&text_for_matching)
            && let Some((command, score)) = scored_commands.get_mut(command_label)
        {
            trace!(
                "Updating score for command '{}' ('{}'): {} -> {}",
                command.display,
                command.command,
                *score,
                *score + score_change
            );
            *score += score_change;
        }
    }
    Scorer::Command {
        command,
        command_label,
        score_change,
    } => {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", command])
            .env("DATA_FILE", temp_file)
            .env("IS_BINARY", if data.is_text() { "0" } else { "1" });
        if data.is_text() {
            cmd.env("TEXT", &text_for_matching);
        }
        let command_succeeded = match cmd.status() {
            Ok(status) => status.success(),
            Err(e) => {
                error!("Failed to execute command for scoring: {e}");
                false
            }
        };
        tracing::info!("command_succeeded: {command_succeeded}");
        if command_succeeded
            && let Some((command, score)) = scored_commands.get_mut(command_label)
        {
            trace!(
                "Command scoring succeeded for '{}' ('{}'): {} -> {}",
                command.display,
                command.command,
                *score,
                *score + score_change
            );
            *score += score_change;
        }
    }
    Scorer::RegexMulti { regex, scores } => {
        if let Ok(re) = Regex::new(regex)
            && re.is_match(&text_for_matching)
        {
            scores.iter().for_each(|(command_label, score_change)| {
                if let Some((command, score)) = scored_commands.get_mut(command_label) {
                    trace!(
                        "Updating score for command '{}' ('{}'): {} -> {}",
                        command.display,
                        command.command,
                        *score,
                        *score + score_change
                    );
                    *score += score_change;
                }
            });
        }
    }
    Scorer::CommandMulti { command, scores } => {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", command])
            .env("DATA_FILE", temp_file)
            .env("IS_BINARY", if data.is_text() { "0" } else { "1" });
        if data.is_text() {
            cmd.env("TEXT", &text_for_matching);
        }
        let command_succeeded = match cmd.status() {
            Ok(status) => status.success(),
            Err(e) => {
                error!("Failed to execute command for scoring: {e}");
                false
            }
        };
        tracing::info!("command_succeeded: {command_succeeded}");
        if command_succeeded {
            scores.iter().for_each(|(command_label, score_change)| {
                if let Some((command, score)) = scored_commands.get_mut(command_label) {
                    trace!(
                        "Command scoring succeeded for '{}' ('{}'): {} -> {}",
                        command.display,
                        command.command,
                        *score,
                        *score + score_change
                    );
                    *score += score_change;
                }
            });
        }
    }
});
    let mut sorted_commands: Vec<_> = scored_commands
        .iter()
        .enumerate()
        .filter(|(_, (_, (_, score)))| *score > 0)
        .collect();
    sorted_commands.sort_by(|a, b| b.1 .1 .1.cmp(&a.1 .1 .1).then_with(|| a.0.cmp(&b.0)));

    match sorted_commands.len() {
        0 => {
            debug!("No scorers matched");
            return Ok(());
        }
        num_cmds
            if (num_cmds == 1 && sorted_commands[0].1 .1 .1 > config.auto_select_min_threshold)
                || (num_cmds >= 2
                    && sorted_commands[0].1 .1 .1
                        > config.auto_select_max_threshold + sorted_commands[1].1 .1 .1
                    && sorted_commands[0].1 .1 .1 > 10) =>
        {
            debug!(
                "Matched auto-select (max threshold: {}, min threshold: {}): {} with score of {}",
                config.auto_select_max_threshold,
                config.auto_select_min_threshold,
                sorted_commands[0].1 .0,
                sorted_commands[0].1 .1 .1
            );
            let (_, (_label, (command, _))) = &sorted_commands[0];
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", &command.command])
                .env("DATA_FILE", temp_file)
                .env("IS_BINARY", if data.is_text() { "0" } else { "1" });

            if data.is_text() {
                cmd.env("TEXT", &text_for_matching);
            }

            cmd.spawn()?;
        }
        _ => {
            let labels: String = sorted_commands
                .iter()
                .map(|(_, (_, (cmd, _)))| cmd.display.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            debug!("Concatenated labels to dmenu: {labels}");

            let mut child = std::process::Command::new("sh")
                .args(["-c", "dmenu -l 20 -c -i -p 'Faucet'"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()?;

            child.stdin.as_mut().unwrap().write_all(labels.as_bytes())?;

            let output = child.wait_with_output()?;
            let selected_label = String::from_utf8(output.stdout)?.trim().to_string();
            let selected_command = scored_commands
                .iter()
                .find(|(_, (cmd, _))| cmd.display == selected_label);

            if let Some((label, (command, _))) = selected_command {
                debug!("Selected command label: {label}");
                let mut cmd = std::process::Command::new("sh");
                cmd.args(["-c", &command.command])
                    .env("DATA_FILE", temp_file)
                    .env("IS_BINARY", if data.is_text() { "0" } else { "1" });

                if data.is_text() {
                    cmd.env("TEXT", &text_for_matching);
                }

                cmd.spawn()?;
            } else {
                debug!("Didn't select a command in dmenu")
            }
        }
    }
    Ok(())
}

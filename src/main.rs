use std::io::Write;
use std::{
    fmt::Display,
    fs::{create_dir_all, read_to_string, File},
    path::PathBuf,
    process::{exit, Command, Stdio},
};

use anstyle::AnsiColor;
use anstyle::{Color, Style};
use anyhow::{anyhow, bail, Context, Ok};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tempfile::{tempdir, TempDir};

const ACCENT: Style = Style::new()
    .bold()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)));

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(default)]
struct LabTask {
    lab: u32,
    task: u32,
}

impl Display for LabTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lab{}: task{}:", self.lab, self.task)
    }
}

impl Default for LabTask {
    fn default() -> Self {
        Self { lab: 3, task: 2 }
    }
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(default)]
struct MailConfig {
    to: String,
    suppress_cc: bool,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            to: "lkp-maintainers@os.rwth-aachen.de".to_owned(),
            suppress_cc: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Serialize)]
#[serde(default)]
struct GitConfig {
    root_commit: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            root_commit: "v6.5.7".to_owned(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Clone, Serialize)]
#[serde(default)]
struct Config {
    test: LabTask,
    mail: MailConfig,
    git: GitConfig,
}

/// run a command with live stdout
fn run_command(cmd: &mut Command) -> anyhow::Result<()> {
    run_command_(cmd, false)?;
    Ok(())
}

/// run a command without live stdout, but capture stdout
fn run_command_stdout(cmd: &mut Command) -> anyhow::Result<String> {
    run_command_(cmd, true)
}

fn run_command_(cmd: &mut Command, capture: bool) -> anyhow::Result<String> {
    println!("run: {cmd:?}");
    if !capture {
        //live print stdout, but no capture anymore (output.stdout is empty)
        cmd.stdout(Stdio::inherit());
    }
    let display_cmd = format!("failed to execute: {cmd:?}");
    let output = cmd.output().with_context(|| display_cmd.clone())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(anyhow!(stderr).context(display_cmd));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if capture {
        println!("{stdout}");
    }
    Ok(stdout.into())
}

fn write_config(config_file_path: PathBuf, config: &Config) -> anyhow::Result<()> {
    let config_dir = config_file_path.parent().unwrap();
    create_dir_all(config_dir).with_context(|| format!("failed to create {config_dir:?}"))?;
    let mut output = File::create(&config_file_path)?;
    write!(output, "{}", basic_toml::to_string(config).unwrap())
        .with_context(|| format!("failed to write {config_file_path:?}"))
}

fn load_config() -> anyhow::Result<Config> {
    let dirs = ProjectDirs::from("dev", "luckyturtle", env!("CARGO_PKG_NAME"))
        .context("no valid home directory path could be retrieved from the operating system")?;
    let config_file_path = dirs.config_dir().join("config.toml");
    println!("{ACCENT}load config from {config_file_path:?}:{ACCENT:#}");
    if !config_file_path.exists() {
        println!("config file do not exist yet. Create default config.\nPlease configure {config_file_path:?} and retry");
        write_config(config_file_path, &Config::default())?;
        exit(1);
    }
    let config_str = read_to_string(&config_file_path)
        .with_context(|| format!("failed to read {config_file_path:?}"))?;
    let config = basic_toml::from_str(&config_str)
        .with_context(|| format!("failed to deserialize config of file {config_file_path:?}"))?;
    //just to make sure that new config options are also present at the config file
    write_config(config_file_path, &config)?;
    Ok(config)
}

///generate patch files and return absoulte Path to patch files
fn create_patchs(tmp_dir: &TempDir, root_commit: String) -> anyhow::Result<Vec<PathBuf>> {
    println!("{ACCENT}create patchs:{ACCENT:#}");
    let count = run_command_stdout(Command::new("git").args([
        "rev-list",
        "--count",
        &format!("{root_commit}..HEAD"),
    ]))?;
    let count: usize = count
        .trim()
        .parse()
        .with_context(|| format!("{count:?} is not a number"))?;
    if count == 0 {
        println!("nothing to submit");
        exit(0);
    }
    run_command(Command::new("git").args([
        "format-patch",
        "--output-directory",
        &tmp_dir.path().to_string_lossy(),
        &format!("-{count}"),
    ]))
    .context("failed to generate patch")?;
    let mut patch_files: Vec<PathBuf> = Vec::with_capacity(count);
    for entry in std::fs::read_dir(tmp_dir)? {
        patch_files.push(tmp_dir.path().join(entry?.path()));
    }
    Ok(patch_files)
}

///patch the subject of the first mail, so the CI know the task
fn patch_first_mail(patch_files: &Vec<PathBuf>, lab_task: LabTask) -> anyhow::Result<()> {
    println!("{ACCENT}modify first patch subject:{ACCENT:#}");
    //find first patch
    let first_patch = patch_files.iter().find(|f| {
        let file = f
            .file_name()
            .with_context(|| format!("failed to get file name of {f:?}"))
            .unwrap()
            .to_string_lossy();
        file.starts_with("0001-") && file.ends_with(".patch")
    });
    let Some(first_patch) = first_patch else {
        bail!(format!("failed to find first patch file (0001-*.patch). aviable patchs files: {patch_files:#?}"));
    };
    //edit first patch file
    let patch =
        read_to_string(first_patch).with_context(|| format!("failed to read {first_patch:?}"))?;
    let mut output = File::create(first_patch)?;
    //a real parser is too much. just inerate over lines
    for line in patch.lines() {
        if line.starts_with("Subject: [PATCH ") {
            let (line_part1, line_part2) = line.split_once("]").unwrap();
            writeln!(output, "{line_part1}] {lab_task} {line_part2}")
        } else {
            writeln!(output, "{}", line)
        }
        .with_context(|| format!("failed to wirte to {first_patch:?}"))?;
    }
    Ok(())
}

fn send_patch(patch_files: &Vec<PathBuf>, mail_config: &MailConfig) -> anyhow::Result<()> {
    // get patch files
    println!("{ACCENT}send mails:{ACCENT:#}");
    let mut cmd = Command::new("git");
    cmd.args(["send-email", "--to", &mail_config.to, "--confirm=never"]);
    if mail_config.suppress_cc {
        cmd.arg("--suppress-cc=all");
    }
    cmd.args(patch_files);
    run_command(&mut cmd)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let config = load_config().context("failed to load config")?;
    let tmp_dir = tempdir()?;
    create_dir_all(&tmp_dir)
        .with_context(|| format!("failed to create dir {:?}", tmp_dir.path()))?;

    let patch_files =
        create_patchs(&tmp_dir, config.git.root_commit).context("failed to create patchs")?;
    patch_first_mail(&patch_files, config.test).context("failed to patch first mail subject")?;
    send_patch(&patch_files, &config.mail).context("failed to send patch per mail")?;

    println!("\n{ACCENT}submit successful{ACCENT:#}");
    Ok(())
}

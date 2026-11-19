mod alert_text;
mod automation;
mod cli_text;
mod config;
mod engine;
mod handover;
mod i18n;
mod interface;
mod monitor;
mod notifications;
mod profile;
mod relay;
mod runtime_text;
mod server_time;
mod storage;
mod subscription;
mod subscription_hwid;
mod subscription_identity;
#[cfg(test)]
#[path = "../tests/unit/fixtures.rs"]
mod test_fixtures;
mod user_service;
mod xray_binary;
mod xray_listeners;

use anyhow::{Context, Result};
use clap::{ArgAction, CommandFactory, Parser, Subcommand};
use cli_text::text;
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::ExitCode,
};

fn template(command: &str) -> String {
    let labels = text();
    let mut help = format!("{{about}}\n\n{}: proxywatch", labels.usage);
    if !command.is_empty() {
        help.push(' ');
        help.push_str(command);
    }
    help.push_str(&format!(" [{}]", labels.options_value));
    match command {
        "" | "help" => help.push_str(&format!(" [{}]", labels.command_value)),
        "configure" => help.push_str(" --stdin [--merge]"),
        "status" => help.push_str(" --json"),
        _ => {}
    }
    help.push_str("\n\n");
    match command {
        "" => help.push_str(&format!("{}:\n{{subcommands}}\n\n", labels.commands)),
        "help" => help.push_str("{positionals}\n\n"),
        _ => {}
    }
    help.push_str(&format!("{}:\n{{options}}", labels.options));
    help
}

#[derive(Parser)]
#[command(version, about = text().about, help_template = template(""),
    disable_help_flag = true, disable_version_flag = true, disable_help_subcommand = true)]
struct Arguments {
    #[arg(long, global = true, value_name = text().directory_value, help = text().directory)]
    directory: Option<PathBuf>,
    #[arg(short = 'h', long, global = true, action = ArgAction::Help, help = text().help)]
    help: Option<bool>,
    #[arg(short = 'V', long, action = ArgAction::Version, help = text().version)]
    version: Option<bool>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(hide = true)]
    ActivateSystemService,
    #[command(about = text().run, help_template = template("run"))]
    Run,
    #[command(about = text().tui, help_template = template("tui"))]
    Tui,
    #[command(about = text().configure, help_template = template("configure"))]
    Configure {
        #[arg(long, required = true, help = text().stdin)]
        stdin: bool,
        #[arg(long, help = text().merge)]
        merge: bool,
    },
    #[command(about = text().status, help_template = template("status"))]
    Status {
        #[arg(long, required = true, help = text().json)]
        json: bool,
    },
    #[command(about = text().help_command, help_template = template("help"))]
    Help {
        #[arg(value_parser = ["run", "tui", "configure", "status", "help"], hide_possible_values = true, value_name = text().command_value, help = text().help_topics)]
        command: Option<String>,
    },
}

// Clap handles help before returning parsed arguments. Resolve the saved language
// first, including the global option on either side of the subcommand.
fn select_language(arguments: &[OsString]) {
    let mut directory = None;
    let mut options = arguments.iter().skip(1);
    while let Some(option) = options.next() {
        if option == "--" {
            break;
        }
        if option == "--directory" {
            directory = options.next().map(PathBuf::from);
        } else if let Some(value) = option.as_bytes().strip_prefix(b"--directory=") {
            directory = Some(PathBuf::from(OsStr::from_bytes(value)));
        }
    }
    #[derive(serde::Deserialize)]
    struct Preference {
        language: i18n::Language,
    }
    // A missing or malformed preference must allow help and configuration repair.
    let preference = storage::Paths::resolve(directory)
        .ok()
        .and_then(|paths| paths.read::<Preference>("settings.json").ok().flatten());
    i18n::set_language(preference.map(|value| value.language).unwrap_or_default());
}

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args_os().collect();
    select_language(&arguments);
    let arguments = Arguments::try_parse_from(arguments).unwrap_or_else(|error| {
        if error.use_stderr() {
            eprintln!("{}", text().arguments);
            std::process::exit(2);
        }
        error.exit()
    });
    match execute(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", text().error);
            ExitCode::FAILURE
        }
    }
}

fn execute(arguments: Arguments) -> Result<()> {
    let command = arguments.command.unwrap_or(Command::Tui);
    let writable_paths = || -> Result<storage::Paths> {
        let paths = storage::Paths::resolve(arguments.directory.clone())?;
        paths.ensure_directory()?;
        Ok(paths)
    };
    match command {
        Command::ActivateSystemService => {
            let paths = storage::Paths::resolve(arguments.directory.clone())?;
            activate_system_service(&paths, arguments.directory.is_some())
        }
        Command::Help { command } => print_help(command.as_deref()),
        Command::Status { .. } => automation::status(
            &storage::Paths::resolve(arguments.directory)?,
            std::io::stdout().lock(),
        ),
        Command::Configure { merge, .. } => {
            automation::configure(&writable_paths()?, std::io::stdin().lock(), merge)
        }
        Command::Run => monitor::run(&writable_paths()?),
        Command::Tui => interface::run(&writable_paths()?, arguments.directory.is_some()),
    }
}

// The installer also reaches this entry point when a previous version saved settings
// in a root console but could not start a service there.
fn activate_system_service(paths: &storage::Paths, explicit_directory: bool) -> Result<()> {
    use runtime_text::message;
    use user_service::{Applied, Scope, UserService};
    if !paths.has_settings() {
        return Ok(());
    }
    let Some(service) = UserService::installed(paths.directory(), explicit_directory)
        .filter(|service| service.scope() == Scope::System)
    else {
        eprintln!("{}", message!(system_activation_unavailable));
        return Ok(());
    };
    if !service.stopped() {
        return Ok(());
    }
    let settings = paths.load_settings()?;
    if paths.lock().is_err() || !handover::conflicts(&settings).is_empty() {
        eprintln!("{}", message!(system_activation_busy));
        return Ok(());
    }
    if xray_binary::inspect_binary(&settings.xray_binary).is_none() {
        eprintln!("{}", message!(system_activation_xray));
        return Ok(());
    }
    anyhow::ensure!(
        matches!(service.apply(true), Applied::Started),
        message!(system_activation_failed)
    );
    println!("{}", message!(system_activation_started));
    Ok(())
}

fn print_help(command: Option<&str>) -> Result<()> {
    let mut definition = Arguments::command();
    definition.build();
    let help = match command {
        Some(command) => definition
            .find_subcommand_mut(command)
            .context(text().arguments)?,
        None => &mut definition,
    };
    help.print_help()?;
    Ok(())
}

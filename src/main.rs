mod automation;
mod config;
mod engine;
mod interface;
mod monitor;
mod notifications;
mod profile;
mod relay;
mod storage;
mod subscription;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Управляет локальным прокси и переключает узлы при сбое"
)]
struct Arguments {
    #[arg(long, global = true, help = "Каталог настроек и истории")]
    directory: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Запустить фоновый сервис в текущем процессе")]
    Run,
    #[command(about = "Открыть настройки и историю")]
    Tui,
    #[command(about = "Записать настройки из JSON")]
    Configure {
        #[arg(
            long,
            required = true,
            help = "Прочитать настройки из стандартного ввода"
        )]
        stdin: bool,
    },
    #[command(about = "Прочитать последнее сохранённое состояние")]
    Status {
        #[arg(long, required = true, help = "Вывести состояние в формате JSON")]
        json: bool,
    },
}

fn main() -> Result<()> {
    let arguments = Arguments::try_parse().unwrap_or_else(|error| {
        if error.use_stderr() {
            eprintln!("Некорректные аргументы. Используйте --help.");
            std::process::exit(2);
        }
        error.exit()
    });
    let paths = storage::Paths::resolve(arguments.directory)?;
    let command = arguments.command.unwrap_or(Command::Tui);
    if !matches!(command, Command::Status { .. }) {
        paths.ensure_directory()?;
    }
    match command {
        Command::Status { .. } => automation::status(&paths, std::io::stdout().lock()),
        Command::Configure { .. } => automation::configure(&paths, std::io::stdin().lock()),
        Command::Run => monitor::run(&paths),
        Command::Tui => interface::run(&paths),
    }
}

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
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let paths = storage::Paths::open(arguments.directory)?;
    match arguments.command.unwrap_or(Command::Tui) {
        Command::Run => monitor::run(&paths),
        Command::Tui => interface::run(&paths),
    }
}

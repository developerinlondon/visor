use clap::Parser;
use visor_runtime::cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Start(args) => visor_runtime::cli::start::execute(args).await,
        Command::Run(args) => visor_runtime::cli::run::execute(&cli.addr, args).await,
        Command::Exec(args) => visor_runtime::cli::exec::execute(&cli.addr, args).await,
        Command::Ps => visor_runtime::cli::ps::execute(&cli.addr).await,
        Command::Stop(args) => visor_runtime::cli::stop::execute(&cli.addr, args).await,
        Command::Shell(args) => visor_runtime::cli::shell::execute(&cli.addr, &args).await,
        Command::Info => visor_runtime::cli::info::execute(&cli.addr).await,
        Command::Volume(cmd) => visor_runtime::cli::volume::execute(cmd),
        Command::Tui => visor_runtime::cli::tui::execute(&cli.addr),
        Command::Service(cmd) => visor_runtime::cli::service::execute(cmd),
        Command::Images(args) => visor_runtime::cli::images::execute(&args, &cli.addr).await,
        Command::Top(args) => visor_runtime::cli::top::execute(&args, &cli.addr).await,
        Command::Console(args) => visor_runtime::cli::console::execute(&args, &cli.addr).await,
        Command::Compose(cmd) => visor_runtime::cli::compose::execute(cmd, &cli.addr).await,
        Command::Network(cmd) => visor_runtime::cli::network::execute(cmd, &cli.addr).await,
        Command::Rm(args) => visor_runtime::cli::rm::execute(&cli.addr, args).await,
        Command::Logs(args) => visor_runtime::cli::logs::execute(&cli.addr, args).await,
        Command::Inspect(args) => visor_runtime::cli::inspect::execute(&cli.addr, args).await,
        Command::Kill(args) => visor_runtime::cli::kill::execute(&cli.addr, args).await,
        Command::Pull(args) => visor_runtime::cli::pull::execute(&cli.addr, args).await,
        Command::Rmi(args) => visor_runtime::cli::rmi::execute(&cli.addr, args).await,
        Command::Restart(args) => visor_runtime::cli::restart::execute(&cli.addr, args).await,
        Command::Build(args) => visor_runtime::cli::build::execute(&cli.addr, args).await,
        Command::Push(args) => visor_runtime::cli::push::execute(&cli.addr, args).await,
        Command::VmWorker(args) => visor_runtime::cli::execute_vm_worker(args).await,
        _ => anyhow::bail!("unsupported command"),
    }
}

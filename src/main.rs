use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use kuroe::{api, auth, db, secrets, startup, update};

#[derive(Parser)]
#[command(name = "kuroe", version, about = "Self-hosted deployment server for Docker hosts")]
struct Cli {
    #[arg(long, env = "KUROE_DATA", default_value = "data", global = true)]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server
    Serve {
        #[arg(long, env = "KUROE_LISTEN", default_value = "0.0.0.0:27863")]
        listen: SocketAddr,
        /// Mark session cookies Secure even on plain http
        #[arg(long, env = "KUROE_SECURE_COOKIES")]
        secure_cookies: bool,
        /// Refuse to start with pending migrations instead of applying them. Run `kuroe migrate` yourself.
        #[arg(long, env = "KUROE_NO_MIGRATE")]
        no_migrate: bool,
    },
    /// Apply pending database migrations
    Migrate,
    /// Admin tasks
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Replace a running kuroe container with a new image (run by the updater container)
    SelfUpdate {
        #[arg(long)]
        target: String,
        #[arg(long)]
        image: String,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Set a new random password for a user and sign out their sessions
    ResetPassword { username: String },
    /// Print the one-time code that creating the owner account needs
    SetupCode,
    /// Plain HTTP access: open to everyone, or local (this machine and HTTPS proxies only)
    Http {
        #[arg(value_parser = ["open", "local"])]
        mode: String,
    },
    /// Seal every saved secret again with a new key
    RotateKey {
        /// File with the new key (64 hex characters). A new key is made when left out.
        #[arg(long)]
        new_key_file: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    match &cli.command {
        Command::SelfUpdate { target, image } => {
            let docker = kuroe::docker::connect()?;
            return update::container::run_updater(&docker, target, image, &cli.data_dir).await;
        }
        Command::Serve { listen, secure_cookies, no_migrate } => {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            let app = match startup::boot(&cli.data_dir, *listen, *secure_cookies, *no_migrate).await {
                Ok(app) => app,
                Err(err) => {
                    tracing::error!("kuroe couldn't start: {err:#}");
                    startup::failed(format!("{err:#}"))
                }
            };
            tracing::info!("listening on {listen}");
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
            return Ok(());
        }
        _ => {}
    }
    let pool = db::connect(&cli.data_dir).await?;
    secrets::install(&cli.data_dir)?;

    match cli.command {
        Command::SelfUpdate { .. } | Command::Serve { .. } => unreachable!("handled above"),
        Command::Migrate => {
            let applied = db::migrate(&pool).await?;
            let sealed = secrets::seal_plain(&pool, secrets::sealer()).await.context("sealing saved secrets")?;
            println!("{applied} migrations applied, {sealed} plain secrets sealed");
        }
        Command::Admin { command: AdminCommand::ResetPassword { username } } => {
            db::require_migrated(&pool).await?;
            let password = auth::random_password();
            auth::reset_password(&pool, &username, &password).await?;
            println!("new password for {username}: {password}");
        }
        Command::Admin { command: AdminCommand::SetupCode } => {
            db::require_migrated(&pool).await?;
            if auth::has_users(&pool).await? {
                eprintln!("kuroe is already set up, so there is no setup code");
            } else {
                println!("{}", auth::setup_code::get_or_create(&cli.data_dir).context("reading the setup code")?);
            }
        }
        Command::Admin { command: AdminCommand::Http { mode } } => {
            db::require_migrated(&pool).await?;
            let access = api::http_access::HttpAccess::parse(&mode).context("mode is open or local")?;
            api::http_access::set(&pool, access).await?;
            match access {
                api::http_access::HttpAccess::Open => println!("plain HTTP is open again, a running kuroe picks this up right away"),
                api::http_access::HttpAccess::Local => println!("plain HTTP now only answers this machine and HTTPS proxies"),
            }
        }
        Command::Admin { command: AdminCommand::RotateKey { new_key_file } } => {
            db::require_migrated(&pool).await?;
            rotate_key(&pool, &cli.data_dir, new_key_file).await?;
        }
    }
    Ok(())
}

async fn rotate_key(pool: &sqlx::SqlitePool, data_dir: &std::path::Path, new_key_file: Option<PathBuf>) -> anyhow::Result<()> {
    let (old_key, source) = secrets::load(data_dir)?;
    let new_key = match &new_key_file {
        Some(path) => secrets::Key::from_hex(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)?,
        None => secrets::Key::generate(),
    };
    anyhow::ensure!(new_key.id != old_key.id, "the new key is the same as the current one");
    let old = secrets::Sealer::new(old_key, None);
    secrets::check_keys(pool, &old).await?;
    let staged = data_dir.join("secret.key.new");
    if new_key_file.is_none() {
        secrets::write_key(&staged, &new_key)?;
    }
    let count = secrets::reseal(pool, &old, &secrets::Sealer::new(new_key.clone(), None)).await?;
    println!("sealed {count} secrets again with key {}", new_key.id);
    match (&source, &new_key_file) {
        (secrets::KeySource::DataDir { path }, None) => std::fs::rename(&staged, path)?,
        (secrets::KeySource::DataDir { path }, Some(_)) => secrets::write_key(path, &new_key)?,
        (secrets::KeySource::File { path }, Some(new)) => {
            println!("Point {} at {} instead of {}.", secrets::KEY_FILE_ENV, new.display(), path.display())
        }
        (secrets::KeySource::File { path }, None) => {
            println!("The new key is in {}. Put it in {}, or point {} at it.", staged.display(), path.display(), secrets::KEY_FILE_ENV)
        }
    }
    println!("Restart kuroe so it uses the new key.");
    Ok(())
}

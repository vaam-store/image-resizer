use anyhow::Result;
use envconfig::Envconfig;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

#[derive(Envconfig, Clone)]
pub struct HealthCheckEnvConfig {
    #[envconfig(from = "HOST", default = "0.0.0.0")]
    pub http_host: String,

    #[envconfig(from = "PORT", default = "3000")]
    pub http_port: u16,

    #[envconfig(from = "TIMEOUT", default = "5")]
    pub http_timeout: u8,
}

fn main() -> Result<()> {
    let config = HealthCheckEnvConfig::init_from_env()?;
    let address = format!("{}:{}", config.http_host, config.http_port);

    let socket_addr: SocketAddr = address.parse()?;

    match TcpStream::connect_timeout(
        &socket_addr,
        Duration::from_secs(config.http_timeout as u64),
    ) {
        Ok(_) => {
            println!("Health check is successful");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Health check failed: {}", e);
            std::process::exit(1);
        }
    }
}

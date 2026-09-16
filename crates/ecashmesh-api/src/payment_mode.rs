//! Safety configuration for payment execution environments.
//!
//! This module is intentionally independent from routing and wallet custody.
//! It prevents accidental execution against production endpoints unless the
//! operator explicitly opts in.

use std::env;

use reqwest::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentEnvironment {
    Simulator,
    Regtest,
    Mainnet,
}

impl PaymentEnvironment {
    pub fn from_env() -> Result<Self, String> {
        match env::var("PAYMENT_ENVIRONMENT")
            .unwrap_or_else(|_| "simulator".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "simulator" => Ok(Self::Simulator),
            "regtest" => Ok(Self::Regtest),
            "mainnet" => Ok(Self::Mainnet),
            value => Err(format!(
                "Unsupported PAYMENT_ENVIRONMENT={value}; expected simulator, regtest, or mainnet"
            )),
        }
    }

    pub const fn allows_execution(self) -> bool {
        matches!(self, Self::Regtest | Self::Mainnet)
    }
}

#[derive(Debug, Clone)]
pub struct PaymentSafetyConfig {
    pub environment: PaymentEnvironment,
    pub execution_enabled: bool,
    pub max_amount_sats: u64,
}

impl PaymentSafetyConfig {
    pub fn from_env() -> Result<Self, String> {
        let environment = PaymentEnvironment::from_env()?;
        let execution_enabled = env::var("ECASHMESH_ENABLE_REAL_PAYMENTS")
            .unwrap_or_else(|_| "false".to_owned())
            .parse::<bool>()
            .map_err(|_| "ECASHMESH_ENABLE_REAL_PAYMENTS must be true or false".to_owned())?;
        let max_amount_sats = env::var("ECASHMESH_MAX_PAYMENT_SATS")
            .unwrap_or_else(|_| "10000".to_owned())
            .parse::<u64>()
            .map_err(|_| "ECASHMESH_MAX_PAYMENT_SATS must be an integer".to_owned())?;
        if execution_enabled && !environment.allows_execution() {
            return Err("Real payments require PAYMENT_ENVIRONMENT=regtest or mainnet".to_owned());
        }
        if max_amount_sats == 0 {
            return Err("ECASHMESH_MAX_PAYMENT_SATS must be greater than zero".to_owned());
        }
        Ok(Self {
            environment,
            execution_enabled,
            max_amount_sats,
        })
    }

    pub fn validate_amount(&self, amount_sats: u64) -> Result<(), String> {
        if amount_sats == 0 {
            return Err("Payment amount must be greater than zero".to_owned());
        }
        if amount_sats > self.max_amount_sats {
            return Err(format!(
                "Payment amount exceeds configured limit of {} sats",
                self.max_amount_sats
            ));
        }
        if !self.execution_enabled {
            return Err("Payment execution is disabled".to_owned());
        }
        Ok(())
    }

    pub fn validate_endpoint(&self, endpoint: &str) -> Result<(), String> {
        let url = Url::parse(endpoint)
            .map_err(|_| "Endpoint must be an absolute HTTP(S) URL".to_owned())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Endpoint must use HTTP(S) without credentials".to_owned());
        }
        let host = url
            .host_str()
            .ok_or("Endpoint must include a host")?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let local = host == "localhost"
            || host.rsplit('.').next() == Some("local")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if self.environment == PaymentEnvironment::Regtest && !local {
            return Err("Regtest endpoints must use localhost, loopback, or .local".to_owned());
        }
        if self.environment == PaymentEnvironment::Mainnet && local {
            return Err(
                "Mainnet execution refuses localhost, loopback, and .local endpoints".to_owned(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulator_cannot_execute() {
        assert!(!PaymentEnvironment::Simulator.allows_execution());
    }

    #[test]
    fn regtest_allows_execution() {
        assert!(PaymentEnvironment::Regtest.allows_execution());
    }

    #[test]
    fn regtest_rejects_a_lookalike_host() {
        let config = PaymentSafetyConfig {
            environment: PaymentEnvironment::Regtest,
            execution_enabled: true,
            max_amount_sats: 10,
        };
        assert!(
            config
                .validate_endpoint("http://localhost.evil.test")
                .is_err()
        );
    }
}

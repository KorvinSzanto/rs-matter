use core::cell::RefCell;
use std::collections::HashMap;

use crate::{
    data_model::cluster_basic_information::BasicInfoConfig,
    error::{Error, ErrorCode},
    transport::pipe::Pipe,
};
use astro_dnssd::{DNSServiceBuilder, RegisteredDnsService};
use log::info;

use super::ServiceMode;

pub struct Mdns<'a> {
    dev_det: &'a BasicInfoConfig<'a>,
    matter_port: u16,
    services: RefCell<HashMap<String, RegisteredDnsService>>,
}

impl<'a> Mdns<'a> {
    pub fn new(
        _id: u16,
        _hostname: &str,
        _ip: [u8; 4],
        _ipv6: Option<[u8; 16]>,
        dev_det: &'a BasicInfoConfig<'a>,
        matter_port: u16,
    ) -> Self {
        Self::native_new(dev_det, matter_port)
    }

    pub fn native_new(dev_det: &'a BasicInfoConfig<'a>, matter_port: u16) -> Self {
        Self {
            dev_det,
            matter_port,
            services: RefCell::new(HashMap::new()),
        }
    }

    pub fn add(&self, name: &str, mode: ServiceMode) -> Result<(), Error> {
        info!("Registering mDNS service {}/{:?}", name, mode);

        let _ = self.remove(name);

        mode.service(self.dev_det, self.matter_port, name, |service| {
            let composite_service_type = if !service.service_subtypes.is_empty() {
                format!(
                    "{}.{},{}",
                    service.service,
                    service.protocol,
                    service.service_subtypes.join(",")
                )
            } else {
                format!("{}.{}", service.service, service.protocol)
            };

            let mut builder = DNSServiceBuilder::new(&composite_service_type, service.port)
                .with_name(service.name);

            for kvs in service.txt_kvs {
                info!("mDNS TXT key {} val {}", kvs.0, kvs.1);
                builder = builder.with_key_value(kvs.0.to_string(), kvs.1.to_string());
            }

            let svc = builder.register().map_err(|_| ErrorCode::MdnsError)?;

            self.services.borrow_mut().insert(service.name.into(), svc);

            Ok(())
        })
    }

    pub fn remove(&self, name: &str) -> Result<(), Error> {
        if self.services.borrow_mut().remove(name).is_some() {
            info!("Deregistering mDNS service {}", name);
        }

        Ok(())
    }
}

pub struct MdnsRunner<'a>(&'a Mdns<'a>);

impl<'a> MdnsRunner<'a> {
    pub const fn new(mdns: &'a Mdns<'a>) -> Self {
        Self(mdns)
    }

    pub async fn run_udp(&mut self) -> Result<(), Error> {
        core::future::pending::<Result<(), Error>>().await
    }

    pub async fn run(&self, _tx_pipe: &Pipe<'_>, _rx_pipe: &Pipe<'_>) -> Result<(), Error> {
        core::future::pending::<Result<(), Error>>().await
    }
}

impl<'a> super::Mdns for Mdns<'a> {
    fn add(&self, service: &str, mode: ServiceMode) -> Result<(), Error> {
        Mdns::add(self, service, mode)
    }

    fn remove(&self, service: &str) -> Result<(), Error> {
        Mdns::remove(self, service)
    }
}

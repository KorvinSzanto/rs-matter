use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use crate::{
    acl::Accessor,
    error::{Error, ErrorCode},
    Matter,
};

use super::{
    core::Transport,
    mrp::ReliableMessage,
    network::Address,
    packet::Packet,
    session::{Session, SessionMgr},
};

pub const MAX_EXCHANGES: usize = 8;

pub type Notification = embassy_sync::signal::Signal<NoopRawMutex, ()>;

#[derive(Debug, PartialEq, Eq, Copy, Clone, Default)]
pub(crate) enum Role {
    #[default]
    Initiator = 0,
    Responder = 1,
}

impl Role {
    pub fn complementary(is_initiator: bool) -> Self {
        if is_initiator {
            Self::Responder
        } else {
            Self::Initiator
        }
    }
}

#[derive(Debug)]
pub(crate) struct ExchangeCtx {
    pub(crate) id: ExchangeId,
    pub(crate) role: Role,
    pub(crate) mrp: ReliableMessage,
    pub(crate) state: ExchangeState,
}

#[derive(Debug, Clone)]
pub(crate) enum ExchangeState {
    Construction {
        rx: *mut Packet<'static>,
        notification: *const Notification,
    },
    Active,
    Acknowledge {
        notification: *const Notification,
    },
    ExchangeSend {
        tx: *const Packet<'static>,
        rx: *mut Packet<'static>,
        notification: *const Notification,
    },
    ExchangeRecv {
        _tx: *const Packet<'static>,
        tx_acknowledged: bool,
        rx: *mut Packet<'static>,
        notification: *const Notification,
    },
    Complete {
        tx: *const Packet<'static>,
        notification: *const Notification,
    },
    CompleteAcknowledge {
        _tx: *const Packet<'static>,
        notification: *const Notification,
    },
    Closed,
}

pub struct ExchangeCtr<'a> {
    pub(crate) exchange: Exchange<'a>,
    pub(crate) construction_notification: &'a Notification,
}

impl<'a> ExchangeCtr<'a> {
    pub const fn id(&self) -> &ExchangeId {
        self.exchange.id()
    }

    pub async fn get(mut self, rx: &mut Packet<'_>) -> Result<Exchange<'a>, Error> {
        let construction_notification = self.construction_notification;

        self.exchange.with_ctx_mut(move |exchange, ctx| {
            if !matches!(ctx.state, ExchangeState::Active) {
                Err(ErrorCode::NoExchange)?;
            }

            let rx: &'static mut Packet<'static> = unsafe { core::mem::transmute(rx) };
            let notification: &'static Notification =
                unsafe { core::mem::transmute(&exchange.notification) };

            ctx.state = ExchangeState::Construction { rx, notification };

            construction_notification.signal(());

            Ok(())
        })?;

        self.exchange.notification.wait().await;

        Ok(self.exchange)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ExchangeId {
    pub id: u16,
    pub session_id: SessionId,
}

impl ExchangeId {
    pub fn load(rx: &Packet) -> Self {
        Self {
            id: rx.proto.exch_id,
            session_id: SessionId::load(rx),
        }
    }
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SessionId {
    pub id: u16,
    pub peer_addr: Address,
    pub peer_nodeid: Option<u64>,
    pub is_encrypted: bool,
}

impl SessionId {
    pub fn load(rx: &Packet) -> Self {
        Self {
            id: rx.plain.sess_id,
            peer_addr: rx.peer,
            peer_nodeid: rx.plain.get_src_u64(),
            is_encrypted: rx.plain.is_encrypted(),
        }
    }
}
pub struct Exchange<'a> {
    pub(crate) id: ExchangeId,
    pub(crate) transport: &'a Transport<'a>,
    pub(crate) notification: Notification,
}

impl<'a> Exchange<'a> {
    pub const fn id(&self) -> &ExchangeId {
        &self.id
    }

    pub fn matter(&self) -> &Matter<'a> {
        self.transport.matter()
    }

    pub fn transport(&self) -> &Transport<'a> {
        self.transport
    }

    pub fn accessor(&self) -> Result<Accessor<'a>, Error> {
        self.with_session(|sess| {
            Ok(Accessor::for_session(
                sess,
                &self.transport.matter().acl_mgr,
            ))
        })
    }

    pub fn with_session_mut<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Session) -> Result<T, Error>,
    {
        self.with_ctx(|_self, ctx| {
            let mut session_mgr = _self.transport.session_mgr.borrow_mut();

            let sess_index = session_mgr
                .get(
                    ctx.id.session_id.id,
                    ctx.id.session_id.peer_addr,
                    ctx.id.session_id.peer_nodeid,
                    ctx.id.session_id.is_encrypted,
                )
                .ok_or(ErrorCode::NoSession)?;

            f(session_mgr.mut_by_index(sess_index).unwrap())
        })
    }

    pub fn with_session<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&Session) -> Result<T, Error>,
    {
        self.with_session_mut(|sess| f(sess))
    }

    pub fn with_session_mgr_mut<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut SessionMgr) -> Result<T, Error>,
    {
        let mut session_mgr = self.transport.session_mgr.borrow_mut();

        f(&mut session_mgr)
    }

    pub async fn initiate(&mut self, fabric_id: u64, node_id: u64) -> Result<Exchange<'a>, Error> {
        self.transport.initiate(fabric_id, node_id).await
    }

    pub async fn acknowledge(&mut self) -> Result<(), Error> {
        let wait = self.with_ctx_mut(|_self, ctx| {
            if !matches!(ctx.state, ExchangeState::Active) {
                Err(ErrorCode::NoExchange)?;
            }

            if ctx.mrp.is_empty() {
                Ok(false)
            } else {
                ctx.state = ExchangeState::Acknowledge {
                    notification: &_self.notification as *const _,
                };
                _self.transport.send_notification.signal(());

                Ok(true)
            }
        })?;

        if wait {
            self.notification.wait().await;
        }

        Ok(())
    }

    pub async fn exchange(&mut self, tx: &Packet<'_>, rx: &mut Packet<'_>) -> Result<(), Error> {
        let tx: &Packet<'static> = unsafe { core::mem::transmute(tx) };
        let rx: &mut Packet<'static> = unsafe { core::mem::transmute(rx) };

        self.with_ctx_mut(|_self, ctx| {
            if !matches!(ctx.state, ExchangeState::Active) {
                Err(ErrorCode::NoExchange)?;
            }

            ctx.state = ExchangeState::ExchangeSend {
                tx: tx as *const _,
                rx: rx as *mut _,
                notification: &_self.notification as *const _,
            };
            _self.transport.send_notification.signal(());

            Ok(())
        })?;

        self.notification.wait().await;

        Ok(())
    }

    pub async fn complete(mut self, tx: &Packet<'_>) -> Result<(), Error> {
        self.send_complete(tx).await
    }

    pub async fn send_complete(&mut self, tx: &Packet<'_>) -> Result<(), Error> {
        let tx: &Packet<'static> = unsafe { core::mem::transmute(tx) };

        self.with_ctx_mut(|_self, ctx| {
            if !matches!(ctx.state, ExchangeState::Active) {
                Err(ErrorCode::NoExchange)?;
            }

            ctx.state = ExchangeState::Complete {
                tx: tx as *const _,
                notification: &_self.notification as *const _,
            };
            _self.transport.send_notification.signal(());

            Ok(())
        })?;

        self.notification.wait().await;

        Ok(())
    }

    fn with_ctx<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&Self, &ExchangeCtx) -> Result<T, Error>,
    {
        let mut exchanges = self.transport.exchanges.borrow_mut();

        let exchange = Transport::get(&mut exchanges, &self.id).ok_or(ErrorCode::NoExchange)?; // TODO

        f(self, exchange)
    }

    fn with_ctx_mut<F, T>(&mut self, f: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Self, &mut ExchangeCtx) -> Result<T, Error>,
    {
        let mut exchanges = self.transport.exchanges.borrow_mut();

        let exchange = Transport::get(&mut exchanges, &self.id).ok_or(ErrorCode::NoExchange)?; // TODO

        f(self, exchange)
    }
}

impl<'a> Drop for Exchange<'a> {
    fn drop(&mut self) {
        let _ = self.with_ctx_mut(|_self, ctx| {
            ctx.state = ExchangeState::Closed;
            _self.transport.send_notification.signal(());

            Ok(())
        });
    }
}

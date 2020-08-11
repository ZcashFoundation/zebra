use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use abscissa_core::{Component, FrameworkError};
use futures::{channel::oneshot, prelude::*};
use tower::{buffer::Buffer, util::BoxService, Service};

use zebra_network::{AddressBook, BoxedStdError, Config, Request, Response};

use super::{inbound::Inbound, tokio::TokioComponent};

enum State {
    Setup {
        config: Option<Config>,
        sender: Option<oneshot::Sender<Arc<Mutex<AddressBook>>>>,
        inbound: Option<Buffer<BoxService<Request, Response, BoxedStdError>, Request>>,
    },
    Ready(Buffer<BoxService<Request, Response, BoxedStdError>, Request>),
}

// required by Component
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::Setup { .. } => f.debug_tuple("State::Setup").finish(),
            State::Ready(_) => f.debug_tuple("State::Ready").finish(),
        }
    }
}

/// Sets up the peer set that makes outbound requests.
#[derive(Component, Debug)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
#[component(inject = "init_inbound(zebrad::components::inbound::Inbound)")]
pub struct Outbound {
    state: State,
}

impl Outbound {
    pub fn new(config: Config) -> Outbound {
        Outbound {
            state: State::Setup {
                config: Some(config),
                sender: None,
                inbound: None,
            },
        }
    }

    pub fn init_inbound(&mut self, component: &mut Inbound) -> Result<(), FrameworkError> {
        if let State::Setup {
            ref mut sender,
            ref mut inbound,
            ..
        } = &mut self.state
        {
            *sender = component.address_book_sender();
            *inbound = Some(component.svc());
        } else {
            unreachable!("init_inbound called after Ready");
        }
        Ok(())
    }

    pub fn init_tokio(&mut self, tokio: &TokioComponent) -> Result<(), FrameworkError> {
        let (config, sender, inbound) = if let State::Setup {
            ref mut config,
            ref mut sender,
            ref mut inbound,
        } = &mut self.state
        {
            (
                config.take().expect("must have config"),
                sender.take().expect("init_tokio before init_inbound"),
                inbound.take().expect("init_tokio before init_inbound"),
            )
        } else {
            unreachable!("init_tokio called after Ready");
        };

        unimplemented!()
    }
}

/*
impl<S, A> Component<A> for Outbound<S>
where
    A: abscissa_core::Application,
    S: Send + 'static,
 {
     fn id(&self) -> abscissa_core::component::Id {
        abscissa_core::component::Id::new("zebrad::components::outbound::Outbound")
     }

     fn version(&self) -> abscissa_core::Version {
        abscissa_core::Version::parse("3.0.0-alpha.0").unwrap()
     }
}
*/

use tower::{buffer::Buffer, util::BoxService, BoxError};

use zebra_chain::parameters::Network;
use zebra_state::{self, Config, Request, Response};

pub fn create_state_service(
    network: Network,
) -> Buffer<BoxService<Request, Response, BoxError>, Request> {
    let state_service = zebra_state::init(Config::ephemeral(), network);

    Buffer::new(state_service, 1)
}

use super::*;

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::Info> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::GetInfo;

    async fn on_message(
        &self,
        _request: datacake_rpc::Request<request::Info>,
    ) -> Result<Self::Reply, Status> {
        self.get_info().map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::BlockChainInfo> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::GetBlockChainInfo;

    async fn on_message(
        &self,
        _request: datacake_rpc::Request<request::BlockChainInfo>,
    ) -> Result<Self::Reply, Status> {
        self.get_blockchain_info()
            .map_err(make_status_error)
            .map(Into::into)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::AddressBalance> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::AddressBalance;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::AddressBalance>,
    ) -> Result<Self::Reply, Status> {
        let request::AddressBalance(address_strings) =
            request.to_owned().map_err(|err| Status {
                code: ErrorCode::InvalidPayload,
                message: err.to_string(),
            })?;

        self.get_address_balance(address_strings)
            .await
            .map_err(make_status_error)
            .map(Into::into)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::SendRawTransaction> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::SentTransactionHash;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::SendRawTransaction>,
    ) -> Result<Self::Reply, Status> {
        let request::SendRawTransaction(raw_transaction_data) =
            request.to_owned().map_err(|err| Status {
                code: ErrorCode::InvalidPayload,
                message: err.to_string(),
            })?;

        self.send_raw_transaction(raw_transaction_data)
            .await
            .map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::Block> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::GetBlock;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::Block>,
    ) -> Result<Self::Reply, Status> {
        let request::Block {
            hash_or_height,
            verbosity,
        } = request.to_owned().map_err(|err| Status {
            code: ErrorCode::InvalidPayload,
            message: err.to_string(),
        })?;

        self.get_block(hash_or_height, verbosity)
            .await
            .map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::BestTipBlockHash> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::GetBlockHash;

    async fn on_message(
        &self,
        _request: datacake_rpc::Request<request::BestTipBlockHash>,
    ) -> Result<Self::Reply, Status> {
        self.get_best_block_hash().map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::RawMempool> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = Vec<String>;

    async fn on_message(
        &self,
        _request: datacake_rpc::Request<request::RawMempool>,
    ) -> Result<Self::Reply, Status> {
        self.get_raw_mempool().await.map_err(make_status_error)
    }
}

#[datacake_rpc::async_trait]
impl<Mempool, State, Tip> Handler<request::RawTransaction> for RpcImpl<Mempool, State, Tip>
where
    Mempool: tower::Service<
            mempool::Request,
            Response = mempool::Response,
            Error = zebra_node_services::BoxError,
        > + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    type Reply = response::GetRawTransaction;

    async fn on_message(
        &self,
        request: datacake_rpc::Request<request::RawTransaction>,
    ) -> Result<Self::Reply, Status> {
        let request::RawTransaction { tx_id_hex, verbose } =
            request.to_owned().map_err(|err| Status {
                code: ErrorCode::InvalidPayload,
                message: err.to_string(),
            })?;

        self.get_raw_transaction(tx_id_hex, verbose)
            .await
            .map_err(make_status_error)
    }
}

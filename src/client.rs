use std::error::Error;
use std::future::Future;

#[derive(Clone)]
pub struct SyncClient {
    node: bazuka::client::PeerAddress,
    network: String,
    miner_token: String,
    sk: <bazuka::core::Signer as bazuka::crypto::SignatureScheme>::Priv,
}

impl SyncClient {
    pub fn new(node: bazuka::client::PeerAddress, network: &str, miner_token: String) -> Self {
        Self {
            node,
            network: network.to_string(),
            miner_token,
            sk: <bazuka::core::Signer as bazuka::crypto::SignatureScheme>::generate_keys(b"dummy")
                .1,
        }
    }
    fn call<
        R,
        Fut: Future<Output = Result<R, Box<dyn Error>>>,
        F: FnOnce(bazuka::client::BazukaClient) -> Fut,
    >(
        &self,
        f: F,
    ) -> Result<R, Box<dyn Error>> {
        Ok(tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let (lp, client) = bazuka::client::BazukaClient::connect(
                    self.sk.clone(),
                    self.node,
                    self.network.clone(),
                    Some(self.miner_token.clone()),
                );

                let (res, _) = tokio::join!(
                    async move { Ok::<_, bazuka::client::NodeError>(f(client).await) },
                    lp
                );

                res
            })??)
    }
    pub fn transact_deposit(
        &self,
        tx: bazuka::core::MpnDeposit,
    ) -> Result<bazuka::client::messages::PostMpnDepositResponse, Box<dyn Error>> {
        self.call(move |client| async move { Ok(client.transact_contract_deposit(tx).await?) })
    }
    pub fn transact_zero(
        &self,
        tx: bazuka::zk::MpnTransaction,
    ) -> Result<bazuka::client::messages::PostMpnTransactionResponse, Box<dyn Error>> {
        self.call(move |client| async move { Ok(client.zero_transact(tx).await?) })
    }
    pub fn get_mpn_account(
        &self,
        index: u64,
    ) -> Result<bazuka::client::messages::GetMpnAccountResponse, Box<dyn Error>> {
        self.call(move |client| async move { Ok(client.get_mpn_account(index).await?) })
    }
    pub fn get_account(
        &self,
        address: bazuka::core::Address,
    ) -> Result<bazuka::client::messages::GetAccountResponse, Box<dyn Error>> {
        self.call(move |client| async move { Ok(client.get_account(address).await?) })
    }
    pub fn get_header(&self, index: u64) -> Result<Option<bazuka::core::Header>, Box<dyn Error>> {
        self.call(move |client| async move {
            Ok(client.get_headers(index, 1).await?.headers.first().cloned())
        })
    }
    pub fn get_height(&self) -> Result<u64, Box<dyn Error>> {
        self.call(move |client| async move { Ok(client.stats().await.map(|resp| resp.height)?) })
    }
}

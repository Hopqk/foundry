//! tests for custom anvil endpoints
use crate::{
    abi::*,
    fork::fork_config,
    utils::{http_provider, http_provider_with_signer},
};
use alloy_network::{EthereumSigner, TransactionBuilder};
use alloy_primitives::{address, Address, U256, U64};
use alloy_provider::Provider;
use alloy_rpc_types::{BlockId::Number, BlockNumberOrTag, TransactionRequest, WithOtherFields};
use alloy_signer::Signer;
use anvil::{eth::api::CLIENT_VERSION, spawn, Hardfork, NodeConfig};
use anvil_core::{
    eth::EthRequest,
    types::{AnvilMetadata, ForkedNetwork, Forking, NodeEnvironment, NodeForkConfig, NodeInfo},
};
use foundry_common::types::{ToAlloy, ToEthers};
use foundry_evm::revm::primitives::SpecId;
use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

#[tokio::test(flavor = "multi_thread")]
async fn can_set_gas_price() {
    let (api, handle) = spawn(NodeConfig::test().with_hardfork(Some(Hardfork::Berlin))).await;

    let provider = http_provider(&handle.http_endpoint());

    let gas_price = U256::from(1337);
    api.anvil_set_min_gas_price(gas_price).await.unwrap();
    assert_eq!(gas_price.to::<u128>(), provider.get_gas_price().await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn can_set_block_gas_limit() {
    let (api, _) = spawn(NodeConfig::test().with_hardfork(Some(Hardfork::Berlin))).await;

    let block_gas_limit = U256::from(1337);
    assert!(api.evm_set_block_gas_limit(block_gas_limit).unwrap());
    // Mine a new block, and check the new block gas limit
    api.mine_one().await;
    let latest_block =
        api.block_by_number(alloy_rpc_types::BlockNumberOrTag::Latest).await.unwrap().unwrap();
    assert_eq!(block_gas_limit.to::<u128>(), latest_block.header.gas_limit);
}

// Ref <https://github.com/foundry-rs/foundry/issues/2341>
#[tokio::test(flavor = "multi_thread")]
async fn can_set_storage() {
    let (api, _handle) = spawn(NodeConfig::test()).await;
    let s = r#"{"jsonrpc": "2.0", "method": "hardhat_setStorageAt", "id": 1, "params": ["0xe9e7CEA3DedcA5984780Bafc599bD69ADd087D56", "0xa6eef7e35abe7026729641147f7915573c7e97b47efa546f5f6e3230263bcb49", "0x0000000000000000000000000000000000000000000000000000000000003039"]}"#;
    let req = serde_json::from_str::<EthRequest>(s).unwrap();
    let (addr, slot, val) = match req.clone() {
        EthRequest::SetStorageAt(addr, slot, val) => (addr, slot, val),
        _ => unreachable!(),
    };

    api.execute(req).await;

    let storage_value = api.storage_at(addr, slot, None).await.unwrap();
    assert_eq!(val, storage_value);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_account() {
    let (api, handle) = spawn(NodeConfig::test()).await;

    let provider = http_provider(&handle.http_endpoint());

    let impersonate = Address::random();
    let to = Address::random();
    let val = U256::from(1337);
    let funding = U256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(impersonate, funding).await.unwrap();

    let balance = api.balance(impersonate, None).await.unwrap();
    assert_eq!(balance, funding);

    let tx = TransactionRequest::default()
        .with_from(impersonate)
        .with_to(Some(to).into())
        .with_value(val);
    let tx = WithOtherFields::new(tx);

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();

    api.anvil_impersonate_account(impersonate).await.unwrap();
    assert!(api.accounts().unwrap().contains(&impersonate));

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();
    assert_eq!(res.from, impersonate);

    let nonce = provider.get_transaction_count(impersonate, None).await.unwrap();
    assert_eq!(nonce, 1u64);

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_stop_impersonating_account(impersonate).await.unwrap();
    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn can_auto_impersonate_account() {
    let (api, handle) = spawn(NodeConfig::test()).await;

    let provider = http_provider(&handle.http_endpoint());

    let impersonate = Address::random();
    let to = Address::random();
    let val = U256::from(1337);
    let funding = U256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(impersonate, funding).await.unwrap();

    let balance = api.balance(impersonate, None).await.unwrap();
    assert_eq!(balance, funding);

    let tx = TransactionRequest::default()
        .with_from(impersonate)
        .with_to(Some(to).into())
        .with_value(val);
    let tx = WithOtherFields::new(tx);

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();

    api.anvil_auto_impersonate_account(true).await.unwrap();

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();
    assert_eq!(res.from, impersonate);

    let nonce = provider.get_transaction_count(impersonate, None).await.unwrap();
    assert_eq!(nonce, 1u64);

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_auto_impersonate_account(false).await.unwrap();
    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();

    // explicitly impersonated accounts get returned by `eth_accounts`
    api.anvil_impersonate_account(impersonate).await.unwrap();
    assert!(api.accounts().unwrap().contains(&impersonate));
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_contract() {
    let (api, handle) = spawn(NodeConfig::test()).await;

    let wallet = handle.dev_wallets().next().unwrap();
    let signer: EthereumSigner = wallet.clone().into();

    let provider = http_provider(&handle.http_endpoint());
    let provider_with_signer = http_provider_with_signer(&handle.http_endpoint(), signer);

    let greeter_contract_builder =
        AlloyGreeter::deploy_builder(&provider_with_signer, "Hello World!".to_string());
    let greeter_contract_address = greeter_contract_builder.deploy().await.unwrap();
    let greeter_contract = AlloyGreeter::new(greeter_contract_address, &provider);

    let impersonate = greeter_contract_address;

    let to = Address::random();
    let val = U256::from(1337);

    // // fund the impersonated account
    api.anvil_set_balance(impersonate, U256::from(1e18 as u64)).await.unwrap();

    let tx =
        TransactionRequest::default().with_from(impersonate).with_to(to.into()).with_value(val);
    let tx = WithOtherFields::new(tx);

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();

    let AlloyGreeter::greetReturn { _0 } = greeter_contract.greet().call().await.unwrap();
    let greeting = _0;
    assert_eq!("Hello World!", greeting);

    api.anvil_impersonate_account(impersonate).await.unwrap();

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();
    assert_eq!(res.from, impersonate);

    let balance = provider.get_balance(to, None).await.unwrap();
    assert_eq!(balance, val.into());

    api.anvil_stop_impersonating_account(impersonate).await.unwrap();

    let res = provider.send_transaction(tx.clone()).await.unwrap().get_receipt().await.unwrap();

    let AlloyGreeter::greetReturn { _0 } = greeter_contract.greet().call().await.unwrap();
    let greeting = _0;
    assert_eq!("Hello World!", greeting);
}

#[tokio::test(flavor = "multi_thread")]
async fn can_impersonate_gnosis_safe() {
    let (api, handle) = spawn(fork_config()).await;
    let provider = http_provider(&handle.http_endpoint());

    // <https://help.safe.global/en/articles/40824-i-don-t-remember-my-safe-address-where-can-i-find-it>
    let safe = address!("A063Cb7CFd8E57c30c788A0572CBbf2129ae56B6");

    let code = provider.get_code_at(safe, Number(BlockNumberOrTag::Latest)).await.unwrap();
    assert!(!code.is_empty());

    api.anvil_impersonate_account(safe).await.unwrap();

    let code = provider.get_code_at(safe, Number(BlockNumberOrTag::Latest)).await.unwrap();
    assert!(!code.is_empty());

    let balance = U256::from(1e18 as u64);
    // fund the impersonated account
    api.anvil_set_balance(safe, balance).await.unwrap();

    let on_chain_balance =
        provider.get_balance(safe, Some(Number(BlockNumberOrTag::Latest))).await.unwrap();
    assert_eq!(on_chain_balance, balance);

    api.anvil_stop_impersonating_account(safe).await.unwrap();

    let code = provider.get_code_at(safe, Number(BlockNumberOrTag::Latest)).await.unwrap();
    // code is added back after stop impersonating
    assert!(!code.is_empty());
}

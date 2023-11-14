#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    ensure_eq, to_json_binary, Addr, Binary, CosmosMsg, Deps, DepsMut, Env, MessageInfo, Response,
    WasmMsg,
};
use cw2::set_contract_version;
use cw_storage_plus::Map;
use cw_utils::{must_pay, NativeBalance};
use kujira::{amount, Denom};
use unstake::controller::{ExecuteMsg, InstantiateMsg, OfferResponse, QueryMsg};
use unstake::helpers::{predict_address, Controller};
use unstake::{broker::Broker, ContractError};

use crate::config::Config;

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:unstake";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

static DELEGATES: Map<Addr, ()> = Map::new("delegates");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;
    let config = Config::from(msg);
    config.save(deps)?;
    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    let config = Config::load(deps.as_ref())?;
    match msg {
        ExecuteMsg::Unstake { max_fee } => {
            let amount = must_pay(&info, &config.ask_denom.to_string())?;
            let broker = Broker::load(deps.storage)?;
            let offer = broker.offer(deps.as_ref(), amount)?;
            if offer.fee.gt(&max_fee) {
                return Err(ContractError::MaxFeeExceeded {});
            };
            broker.accept_offer(deps, &offer)?;
            let send_msg = config.offer_denom.send(&info.sender, &offer.amount);
            let borrow_message = vault_borrow_msg(&config.vault_address, offer.amount)?;
            let callback_msg = Controller(env.contract.address)
                .call(ExecuteMsg::UnstakeCallback { offer }, vec![])?;

            Ok(Response::default()
                .add_message(send_msg)
                .add_message(borrow_msg)
                .add_message(callback_msg))
        }
        ExecuteMsg::UnstakeCallback { offer } => {
            ensure_eq!(
                info.sender,
                env.contract.address,
                ContractError::Unauthorized {}
            );
            let funds = deps.querier.query_all_balances(env.contract.address)?;

            let label: String = format!(
                "Unstake.fi delegate {}/{}",
                env.block.height,
                env.transaction.map(|x| x.index).unwrap_or_default()
            );

            let (address, salt) =
                predict_address(config.delegate_code_id, &label, &deps.as_ref(), &env)?;

            let msg = unstake::delegate::InstantiateMsg {
                controller: env.contract.address.clone(),
                offer: offer.clone(),
            };

            let instantiate: WasmMsg = WasmMsg::Instantiate2 {
                admin: Some(env.contract.address.into()),
                code_id: config.delegate_code_id,
                label,
                msg: to_json_binary(&msg)?,
                funds,
                salt,
            };

            DELEGATES.save(deps.storage, address, &())?;

            Ok(Response::default())
        }
        ExecuteMsg::Complete { offer } => {
            DELEGATES
                .load(deps.storage, info.sender)
                .map_err(|_| ContractError::Unauthorized {})?;
            DELEGATES.remove(deps.storage, info.sender);

            let debt_tokens = amount(&config.debt_denom(), info.funds.clone())?;
            let returned_tokens = amount(&config.offer_denom, info.funds)?;
            let mut funds = NativeBalance(vec![
                config.debt_denom().coin(&debt_tokens),
                config.offer_denom.coin(&returned_tokens),
            ]);
            funds.normalize();
            let broker = Broker::load(deps.storage)?;
            broker.close_offer(deps, &offer, debt_tokens, returned_tokens)?;
            let ghost_repay_msg: CosmosMsg = CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: config.vault_address.to_string(),
                msg: to_json_binary(&kujira::ghost::receipt_vault::RepayMsg { callback: None })?,
                funds: funds.into_vec(),
            });

            Ok(Response::default().add_message(ghost_repay_msg))
        }
    }
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> Result<Binary, ContractError> {
    match msg {
        QueryMsg::Offer { amount } => {
            let denom = Denom::from("TODO");
            let broker = Broker::load(deps.storage)?;
            let offer = broker.offer(deps, amount)?;
            Ok(to_json_binary(&OfferResponse::from(offer))?)
        }
    }
}

pub fn vault_borrow_msg(addr: &Addr, amount: Uint128) -> StdResult<CosmosMsg> {
    Ok(CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: addr.to_string(),
        msg: to_binary(&VaultExecuteMsg::Borrow(BorrowMsg {
            amount,
            callback: None,
        }))?,
        funds: vec![],
    }))
}

pub fn vault_repay_msg(addr: &Addr, coins: Vec<Coin>) -> StdResult<CosmosMsg> {
    Ok(CosmosMsg::Wasm(WasmMsg::Execute {
        contract_addr: addr.to_string(),
        msg: to_binary(&VaultExecuteMsg::Repay(RepayMsg { callback: None }))?,
        funds: coins,
    }))
}

#[cfg(test)]
mod tests {}

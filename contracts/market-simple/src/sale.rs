use crate::*;
use near_sdk::borsh::{self};

#[derive(BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Bid {
    pub owner_id: AccountId,
    pub price: U128,
}

#[derive(BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Sale {
    pub owner_id: AccountId,
    pub approval_id: U64,
    pub conditions: HashMap<FungibleTokenId, U128>,
    pub bids: HashMap<FungibleTokenId, Bid>,
}

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Price {
    pub price: Option<U128>,
    pub ft_token_id: Option<AccountId>,
}

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct SaleArgs {
    pub prices: Vec<Price>,
}

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct PurchaseArgs {
    pub nft_contract_id: AccountId,
    pub token_id: TokenId,
}

#[near_bindgen]
impl Contract {
    #[payable]
    pub fn add_sale(
        &mut self,
        nft_contract_id: ValidAccountId,
        token_id: String,
        owner_id: ValidAccountId,
        approval_id: U64,
        sale_args: SaleArgs,
    ) {
        assert!(
            self.storage_deposits.contains(owner_id.as_ref()),
            "Must call storage_deposit with {} to sell on this market.",
            STORAGE_AMOUNT
        );
        let contract_id: AccountId = nft_contract_id.into();

        let SaleArgs {
            prices
        } = sale_args;

        let mut conditions = HashMap::new();

        for item in prices {
            let Price{
                price,
                ft_token_id,
            } = item;
            if let Some(ft_token_id) = ft_token_id {
                if let Some(price) = price {
                    // sale is denominated in FT
                    conditions.insert(ft_token_id, price);
                } else {
                    // accepting bids
                    conditions.insert(ft_token_id, U128(0));
                }
            } else {
                // sale denom in NEAR
                if let Some(price) = price {
                    conditions.insert("near".to_string(), price);
                } else {
                    conditions.insert("near".to_string(), U128(0));
                }
            }
        }
        
        env::log(format!("add_sale for owner: {}", owner_id.as_ref()).as_bytes());

        let bids = HashMap::new();

        self.sales.insert(
            &format!("{}:{}", contract_id, token_id),
            &Sale {
                owner_id: owner_id.into(),
                approval_id,
                conditions,
                bids,
            },
        );
    }

    pub fn update_price(
        &mut self,
        nft_contract_id: ValidAccountId,
        token_id: String,
        ft_token_id: ValidAccountId,
        price: U128,
    ) {
        let contract_id: AccountId = nft_contract_id.into();
        let contract_and_token_id = format!("{}:{}", contract_id, token_id);
        let mut sale = self.sales.get(&contract_and_token_id).expect("No sale");
        assert_eq!(
            env::predecessor_account_id(),
            sale.owner_id,
            "Must be sale owner"
        );
        sale.conditions.insert(ft_token_id.into(), price);
        self.sales.insert(&contract_and_token_id, &sale);
    }

    /// should be able to pull a sale without yocto redirect to wallet?
    pub fn remove_sale(&mut self, nft_contract_id: ValidAccountId, token_id: String) {
        let contract_id: AccountId = nft_contract_id.into();
        let sale = self
            .sales
            .remove(&format!("{}:{}", contract_id, token_id))
            .expect("No sale");
        assert_eq!(
            env::predecessor_account_id(),
            sale.owner_id,
            "Must be sale owner"
        );
    }

    #[payable]
    pub fn purchase(
        &mut self,
        nft_contract_id: ValidAccountId,
        token_id: String,
    ) -> Promise {
        let contract_id: AccountId = nft_contract_id.into();
        let contract_and_token_id = format!("{}:{}", contract_id, token_id);
        let sale = self.sales.get(&contract_and_token_id).expect("No sale");
        let near_token_id = "near".to_string();
        let price = sale.conditions.get(&near_token_id).expect("Not for sale in NEAR");
        let deposit = env::attached_deposit();
        assert_eq!(
            env::attached_deposit(),
            u128::from(*price),
            "Must pay exactly the sale amount {}",
            deposit
        );
        self.process_purchase(contract_id, token_id, near_token_id, env::predecessor_account_id())
    }

    #[private]
    pub fn process_purchase(
        &mut self,
        nft_contract_id: AccountId,
        token_id: String,
        ft_token_id: AccountId,
        buyer_id: AccountId,
    ) -> Promise {
        let contract_id: AccountId = nft_contract_id.into();
        let contract_and_token_id = format!("{}:{}", contract_id, token_id);
        let sale = self.sales.remove(&contract_and_token_id).expect("No sale");
        let price = *sale.conditions.get(&ft_token_id).unwrap();

        ext_contract::nft_transfer(
            buyer_id.clone(),
            token_id.clone(),
            sale.owner_id.clone(),
            None,
            price,
            &contract_id,
            1,
            GAS_FOR_NFT_TRANSFER,
        )
        .then(ext_self::resolve_purchase(
            ft_token_id,
            buyer_id,
            sale,
            &env::current_account_id(),
            NO_DEPOSIT,
            GAS_FOR_ROYALTIES,
        ))
    }

    /// self callback

    #[private]
    pub fn resolve_purchase(
        &mut self,
        ft_token_id: AccountId,
        buyer_id: AccountId,
        sale: Sale,
    ) -> U128 {

        let price = *sale.conditions.get(&ft_token_id).unwrap();

        // checking for payout information
        let payout_option = match env::promise_result(0) {
            PromiseResult::NotReady => unreachable!(),
            PromiseResult::Successful(value) => {
                // None means a bad payout from bad NFT contract
                near_sdk::serde_json::from_slice::<Payout>(&value).ok().and_then(|payout| {
                    // gas to do 8 FT transfers (and definitely 8 NEAR transfers)
                    if payout.len() > 8 {
                        None
                    } else {
                        // payouts must == sale.price, otherwise something wrong with NFT contract
                        // TODO off by 1 e.g. payouts are fractions of 3333 + 3333 + 3333
                        let sum: u128 = payout.values().map(|a| *a).reduce(|a, b| a + b).unwrap();
                        if sum == u128::from(price) {
                            Some(payout)
                        } else {
                            None
                        }
                    }
                })
            }
            PromiseResult::Failed => {
                None
            }
        };

        // is payout option valid?
        let payout = if let Some(payout_option) = payout_option {
            payout_option
        } else {
            env::log(format!("Refunding {} to {}", u128::from(price), buyer_id).as_bytes());
            // refund NEAR
            if ft_token_id == "near" {
                Promise::new(buyer_id).transfer(u128::from(price));
            }
            // leave function and return all FTs in ft_resolve_transfer
            return price;
        };

        env::log(format!("Royalty {:?}", payout).as_bytes());

        // NEAR payouts
        if ft_token_id == "near" {
            for (receiver_id, amount) in &payout {
                env::log(format!("NEAR payout payment {} to {}", *amount, receiver_id).as_bytes());
                Promise::new(receiver_id.to_string()).transfer(*amount);
            }
            // refund all FTs (won't be any)
            price
        } else {
        // FT payouts
            for (receiver_id, amount) in &payout {
                env::log(format!("FT payout payment {} to {}", *amount, receiver_id).as_bytes());
                ext_contract::ft_transfer(
                    receiver_id.to_string(),
                    U128(*amount),
                    None,
                    &ft_token_id,
                    1,
                    GAS_FOR_FT_TRANSFER,
                );
            }
            // keep all FTs (already transferred for payouts)
            U128(0)
        }
    }                             

    pub fn get_sale(&self, nft_contract_id: ValidAccountId, token_id: String) -> Sale {
        let contract_id: AccountId = nft_contract_id.into();
        self.sales
            .get(&format!("{}:{}", contract_id, token_id))
            .expect("No sale")
    }
}

/// self call

#[ext_contract(ext_self)]
trait ExtSelf {
    fn resolve_purchase(
        &mut self,
        ft_token_id: AccountId,
        buyer_id: AccountId,
        sale: Sale,
    ) -> Promise;
}

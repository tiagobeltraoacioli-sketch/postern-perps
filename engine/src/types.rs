//! Canonical data types (SPEC sections 2 and 3).
use crate::hash::{Addr, H256};

pub const ORDER_LIMIT: u8 = 0;
pub const ORDER_MARKET: u8 = 1;
pub const ORDER_STOP_MARKET: u8 = 2;
pub const ORDER_STOP_LIMIT: u8 = 3;
pub const ORDER_TP_MARKET: u8 = 4;
pub const ORDER_TP_LIMIT: u8 = 5;

pub const TIF_GTC: u8 = 0;
pub const TIF_IOC: u8 = 1;
pub const TIF_POST_ONLY: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceOrder {
    pub account: Addr,
    pub nonce: u64,
    pub market_id: u32,
    pub is_buy: bool,
    pub order_type: u8,
    pub time_in_force: u8,
    pub price: u64,
    pub size: u64,
    pub reduce_only: bool,
    pub trigger_price: u64,
    pub client_order_id: H256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    PlaceOrder(PlaceOrder),
    CancelOrder { account: Addr, nonce: u64, market_id: u32, order_id: u64 },
    CancelAll { account: Addr, nonce: u64, market_id: u32 },
    UpdateLeverage { account: Addr, nonce: u64, market_id: u32, leverage: u32, isolated: bool },
    UpdateMargin { account: Addr, nonce: u64, market_id: u32, amount: u64, add: bool },
    Withdraw { account: Addr, nonce: u64, amount: u64, recipient: Addr },
    ApproveAgent { account: Addr, nonce: u64, agent: Addr, name: String, expires_at: u64 },
    RevokeAgent { account: Addr, nonce: u64, agent: Addr },
}

impl Action {
    pub fn type_id(&self) -> u8 {
        match self {
            Action::PlaceOrder(_) => 1,
            Action::CancelOrder { .. } => 2,
            Action::CancelAll { .. } => 3,
            Action::UpdateLeverage { .. } => 4,
            Action::UpdateMargin { .. } => 5,
            Action::Withdraw { .. } => 6,
            Action::ApproveAgent { .. } => 7,
            Action::RevokeAgent { .. } => 8,
        }
    }
    pub fn account(&self) -> Addr {
        match self {
            Action::PlaceOrder(p) => p.account,
            Action::CancelOrder { account, .. }
            | Action::CancelAll { account, .. }
            | Action::UpdateLeverage { account, .. }
            | Action::UpdateMargin { account, .. }
            | Action::Withdraw { account, .. }
            | Action::ApproveAgent { account, .. }
            | Action::RevokeAgent { account, .. } => *account,
        }
    }
    pub fn nonce(&self) -> u64 {
        match self {
            Action::PlaceOrder(p) => p.nonce,
            Action::CancelOrder { nonce, .. }
            | Action::CancelAll { nonce, .. }
            | Action::UpdateLeverage { nonce, .. }
            | Action::UpdateMargin { nonce, .. }
            | Action::Withdraw { nonce, .. }
            | Action::ApproveAgent { nonce, .. }
            | Action::RevokeAgent { nonce, .. } => *nonce,
        }
    }
    /// S-2.4: may an approved agent sign this type?
    pub fn agent_allowed(&self) -> bool {
        matches!(
            self,
            Action::PlaceOrder(_)
                | Action::CancelOrder { .. }
                | Action::CancelAll { .. }
                | Action::UpdateLeverage { .. }
                | Action::UpdateMargin { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedAction {
    pub action: Action,
    pub signature: [u8; 65],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    pub market_id: u32,
    pub price: u64,
    pub sources: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedObservation {
    pub signer: u8,
    pub time_ms: u64,
    pub prices: Vec<OraclePrice>,
    pub signature: [u8; 65],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleUpdate {
    pub round: u64,
    pub observations: Vec<SignedObservation>,
}

/// Forced request kinds = VAULT.md 0.2 `requestForced(uint8 kind, uint64 arg)` (390, 64072cb).
pub const FORCED_CANCEL_ALL: u8 = 0;
/// Close every position.
pub const FORCED_CLOSE: u8 = 1;
/// Withdraw `arg` gross (0 = everything withdrawable) to the account itself.
pub const FORCED_WITHDRAW: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Oracle(OracleUpdate),
    Deposit { index: u64, account: Addr, amount: u64 },
    /// `deadline` is the vault's (L2 `block.timestamp`, seconds): carried for the forced chain head only; the vault
    /// enforces it (SPEC section 14).
    Forced { index: u64, account: Addr, kind: u8, deadline: u64, arg: u64 },
    Action(SignedAction),
}

impl Input {
    pub fn tag(&self) -> u8 {
        match self {
            Input::Oracle(_) => 0x01,
            Input::Deposit { .. } => 0x02,
            Input::Forced { .. } => 0x03,
            Input::Action(_) => 0x04,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub height: u64,
    pub time: u64,
    pub inputs: Vec<Input>,
}

/// Error names of SPEC section 13 (passed through by the API).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Error {
    BadSignature,
    AgentNotAllowed,
    AgentInvalid,
    NonceWindow,
    NonceUsed,
    Malformed,
    UnknownMarket,
    MarketHalted,
    Tick,
    Step,
    SizeBounds,
    WouldCross,
    ReduceOnly,
    InsufficientMargin,
    TooManyOrders,
    OiCap,
    NoLiquidity,
    OrderNotFound,
    Leverage,
    NotIsolated,
    IsolatedUnsupported,
    WithdrawTooSmall,
    PositionInHaltedMarket,
    Overflow,
    DepositOrder,
    ForcedOrder,
    UnknownAccount,
    OracleRoundNotNew,
    OracleMalformed,
    OracleBadSignature,
    OracleTime,
    SelfTrade,
    Cancelled,
    NotLaunched,
    /// S-9.2.1 (R395-22): the vault, the collateral token or a keyless low address as withdrawal recipient.
    BadRecipient,
}

impl Error {
    pub fn name(&self) -> &'static str {
        use Error::*;
        match self {
            BadSignature => "BAD_SIGNATURE",
            AgentNotAllowed => "AGENT_NOT_ALLOWED",
            AgentInvalid => "AGENT_INVALID",
            NonceWindow => "NONCE_WINDOW",
            NonceUsed => "NONCE_USED",
            Malformed => "MALFORMED",
            UnknownMarket => "UNKNOWN_MARKET",
            MarketHalted => "MARKET_HALTED",
            Tick => "TICK",
            Step => "STEP",
            SizeBounds => "SIZE_BOUNDS",
            WouldCross => "WOULD_CROSS",
            ReduceOnly => "REDUCE_ONLY",
            InsufficientMargin => "INSUFFICIENT_MARGIN",
            TooManyOrders => "TOO_MANY_ORDERS",
            OiCap => "OI_CAP",
            NoLiquidity => "NO_LIQUIDITY",
            OrderNotFound => "ORDER_NOT_FOUND",
            Leverage => "LEVERAGE",
            NotIsolated => "NOT_ISOLATED",
            IsolatedUnsupported => "ISOLATED_UNSUPPORTED",
            WithdrawTooSmall => "WITHDRAW_TOO_SMALL",
            PositionInHaltedMarket => "POSITION_IN_HALTED_MARKET",
            Overflow => "OVERFLOW",
            DepositOrder => "DEPOSIT_ORDER",
            ForcedOrder => "FORCED_ORDER",
            UnknownAccount => "UNKNOWN_ACCOUNT",
            OracleRoundNotNew => "ORACLE_ROUND_NOT_NEW",
            OracleMalformed => "ORACLE_MALFORMED",
            OracleBadSignature => "ORACLE_BAD_SIGNATURE",
            OracleTime => "ORACLE_TIME",
            SelfTrade => "SELF_TRADE",
            Cancelled => "CANCELLED",
            NotLaunched => "NOT_LAUNCHED",
            BadRecipient => "BAD_RECIPIENT",
        }
    }
}

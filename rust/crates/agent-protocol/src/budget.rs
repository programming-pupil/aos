use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum BudgetDimension {
    TokenInput,
    TokenOutput,
    UsdMicros,
    WallTimeMs,
    ToolCalls,
    WebQueries,
    DatasourceScans,
    ChildSlots,
    ArtifactBytes,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BudgetState {
    pub available: u64,
    pub reserved: u64,
    pub committed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BudgetReservation {
    pub id: String,
    pub owner: String,
    pub amounts: BTreeMap<BudgetDimension, u64>,
    pub active: bool,
    pub parent_id: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("insufficient {dimension:?} budget: requested {requested}, available {available}")]
    Insufficient {
        dimension: BudgetDimension,
        requested: u64,
        available: u64,
    },
    #[error("unknown or inactive reservation {0}")]
    UnknownReservation(String),
    #[error("committed amount {committed} exceeds reservation {reserved}")]
    CommitExceedsReservation { committed: u64, reserved: u64 },
    #[error("budget arithmetic overflow")]
    Overflow,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetLedger {
    accounts: BTreeMap<BudgetDimension, BudgetState>,
    reservations: BTreeMap<String, BudgetReservation>,
}

impl BudgetLedger {
    pub fn new<I>(initial: I) -> Self
    where
        I: IntoIterator<Item = (BudgetDimension, u64)>,
    {
        Self {
            accounts: initial
                .into_iter()
                .map(|(d, v)| {
                    (
                        d,
                        BudgetState {
                            available: v,
                            ..BudgetState::default()
                        },
                    )
                })
                .collect(),
            reservations: BTreeMap::new(),
        }
    }
    pub fn state(&self, dimension: BudgetDimension) -> BudgetState {
        self.accounts.get(&dimension).copied().unwrap_or_default()
    }
    pub fn reserve<I>(
        &mut self,
        owner: impl Into<String>,
        amounts: I,
    ) -> Result<BudgetReservation, BudgetError>
    where
        I: IntoIterator<Item = (BudgetDimension, u64)>,
    {
        let amounts: BTreeMap<_, _> = amounts.into_iter().collect();
        for (&dimension, &amount) in &amounts {
            let state = self.state(dimension);
            if amount > state.available {
                return Err(BudgetError::Insufficient {
                    dimension,
                    requested: amount,
                    available: state.available,
                });
            }
        }
        for (&dimension, &amount) in &amounts {
            let state = self.accounts.entry(dimension).or_default();
            state.available = state
                .available
                .checked_sub(amount)
                .ok_or(BudgetError::Overflow)?;
            state.reserved = state
                .reserved
                .checked_add(amount)
                .ok_or(BudgetError::Overflow)?;
        }
        let reservation = BudgetReservation {
            id: Uuid::new_v4().to_string(),
            owner: owner.into(),
            amounts,
            active: true,
            parent_id: None,
        };
        self.reservations
            .insert(reservation.id.clone(), reservation.clone());
        Ok(reservation)
    }
    pub fn reserve_child<I>(
        &mut self,
        parent: &BudgetReservation,
        owner: impl Into<String>,
        amounts: I,
    ) -> Result<BudgetReservation, BudgetError>
    where
        I: IntoIterator<Item = (BudgetDimension, u64)>,
    {
        let amounts: BTreeMap<_, _> = amounts.into_iter().collect();
        if !parent.active {
            return Err(BudgetError::UnknownReservation(parent.id.clone()));
        }
        for (&dimension, &amount) in &amounts {
            let reserved = parent.amounts.get(&dimension).copied().unwrap_or(0);
            let already_child: u64 = self
                .reservations
                .values()
                .filter(|r| r.parent_id.as_deref() == Some(&parent.id) && r.active)
                .map(|r| r.amounts.get(&dimension).copied().unwrap_or(0))
                .sum();
            if amount > reserved.saturating_sub(already_child) {
                return Err(BudgetError::Insufficient {
                    dimension,
                    requested: amount,
                    available: reserved.saturating_sub(already_child),
                });
            }
        }
        // Parent allocation is already reserved, so child allocation is a
        // ledger sub-allocation rather than a second debit from availability.
        let reservation = BudgetReservation {
            id: Uuid::new_v4().to_string(),
            owner: owner.into(),
            amounts,
            active: true,
            parent_id: Some(parent.id.clone()),
        };
        self.reservations
            .insert(reservation.id.clone(), reservation.clone());
        Ok(reservation)
    }
    pub fn commit<I>(
        &mut self,
        reservation: &BudgetReservation,
        actual: I,
    ) -> Result<(), BudgetError>
    where
        I: IntoIterator<Item = (BudgetDimension, u64)>,
    {
        let stored = self
            .reservations
            .get_mut(&reservation.id)
            .ok_or_else(|| BudgetError::UnknownReservation(reservation.id.clone()))?;
        if !stored.active {
            return Err(BudgetError::UnknownReservation(reservation.id.clone()));
        }
        let actual: BTreeMap<_, _> = actual.into_iter().collect();
        for (&d, &used) in &actual {
            let reserved = stored.amounts.get(&d).copied().unwrap_or(0);
            if used > reserved {
                return Err(BudgetError::CommitExceedsReservation {
                    committed: used,
                    reserved,
                });
            }
        }
        if stored.parent_id.is_some() {
            stored.active = false;
            return Ok(());
        }
        for (&d, &reserved) in &stored.amounts {
            let used = actual.get(&d).copied().unwrap_or(0);
            let state = self.accounts.entry(d).or_default();
            state.reserved = state.reserved.saturating_sub(reserved);
            state.committed = state
                .committed
                .checked_add(used)
                .ok_or(BudgetError::Overflow)?;
            state.available = state
                .available
                .checked_add(reserved - used)
                .ok_or(BudgetError::Overflow)?;
        }
        stored.active = false;
        Ok(())
    }
    pub fn release(&mut self, reservation: &BudgetReservation) -> Result<(), BudgetError> {
        self.commit(reservation, std::iter::empty())
    }
}

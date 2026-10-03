use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::{Error, FamilyDeclaration, FamilyIdentity, Result, TableName};

/// Where a family identity stands against the catalog.
pub(crate) enum FamilyRegistration {
    Existing,
    New(TableRegistration),
}

/// The engine's registered family inventory. Each registration binds
/// one family identity to its current table coordinate; the derived
/// store-level schema hash folds over this inventory.
#[derive(Debug, Clone)]
pub struct Catalog {
    registrations: Vec<TableRegistration>,
}

impl Catalog {
    pub fn new(registrations: Vec<TableRegistration>) -> Self {
        Self { registrations }
    }

    pub fn is_registered(&self, name: &TableName) -> bool {
        self.registration_for_table(name.as_str()).is_some()
    }

    /// The persisted family identity bound to a table name, if any.
    pub fn family_identity(&self, name: &TableName) -> Option<&FamilyIdentity> {
        self.registration_for_table(name.as_str())
            .map(TableRegistration::identity)
    }

    /// The registration already carrying this family version — same
    /// family name and schema hash — regardless of table coordinate.
    pub fn registration_for_family(&self, identity: &FamilyIdentity) -> Option<&TableRegistration> {
        self.registrations
            .iter()
            .find(|registration| registration.identity().shares_family(identity))
    }

    /// Replace the registration bound at a table coordinate with an
    /// evolved family identity. This is the in-memory half of a family
    /// evolution; the engine lands the durable half — the catalog row,
    /// the rewritten rows, and the log entries — in one write
    /// transaction.
    pub fn evolve(&mut self, table_name: &str, registration: TableRegistration) -> Result<()> {
        let slot = self
            .registrations
            .iter_mut()
            .find(|existing| existing.table_name() == table_name)
            .ok_or_else(|| Error::TableNotRegistered {
                table: table_name.to_owned(),
            })?;
        *slot = registration;
        Ok(())
    }

    pub fn insert(&mut self, registration: TableRegistration) -> Result<()> {
        if self
            .registration_for_table(registration.table_name())
            .is_some()
        {
            return Err(Error::TableAlreadyRegistered {
                table: registration.table_name().to_owned(),
            });
        }
        self.registrations.push(registration);
        Ok(())
    }

    /// Whether a family identity is already registered here, is new, or
    /// conflicts with what the catalog binds (a different identity at its
    /// table, or its family version already bound to another table).
    pub(crate) fn registration_state(
        &self,
        identity: &FamilyIdentity,
    ) -> Result<FamilyRegistration> {
        if let Some(stored) = self.registration_for_table(identity.table_name()) {
            if stored.identity() != identity {
                return Err(Error::FamilyIdentityMismatch {
                    table: identity.table_name().to_owned(),
                    stored: stored.identity().to_string(),
                    declared: identity.to_string(),
                });
            }
            return Ok(FamilyRegistration::Existing);
        }
        if let Some(bound) = self.registration_for_family(identity) {
            return Err(Error::FamilyAlreadyBound {
                family: identity.family().as_str().to_owned(),
                existing: bound.table_name().to_owned(),
                table: identity.table_name().to_owned(),
            });
        }
        Ok(FamilyRegistration::New(TableRegistration::new(
            identity.clone(),
        )))
    }

    /// Admit one family an open declares: a new family joins this catalog
    /// and is returned for the open's write transaction to persist; an
    /// existing one, or a stored prior generation the declaration evolves
    /// from (left for `register_table`), adds nothing; a conflict is the
    /// typed error, raised before the open writes anything.
    pub(crate) fn admit(
        &mut self,
        declaration: &FamilyDeclaration,
    ) -> Result<Option<TableRegistration>> {
        match self.registration_state(declaration.identity()) {
            Ok(FamilyRegistration::Existing) => Ok(None),
            Ok(FamilyRegistration::New(registration)) => {
                self.insert(registration.clone())?;
                Ok(Some(registration))
            }
            Err(mismatch @ Error::FamilyIdentityMismatch { .. }) => {
                let evolves = self
                    .registration_for_table(declaration.identity().table_name())
                    .is_some_and(|stored| declaration.evolves_from(stored.identity()));
                if evolves { Ok(None) } else { Err(mismatch) }
            }
            Err(other) => Err(other),
        }
    }

    pub fn registrations(&self) -> &[TableRegistration] {
        &self.registrations
    }

    fn registration_for_table(&self, table_name: &str) -> Option<&TableRegistration> {
        self.registrations
            .iter()
            .find(|registration| registration.table_name() == table_name)
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct TableRegistration {
    identity: FamilyIdentity,
}

impl TableRegistration {
    pub fn new(identity: FamilyIdentity) -> Self {
        Self { identity }
    }

    pub fn identity(&self) -> &FamilyIdentity {
        &self.identity
    }

    pub fn table_name(&self) -> &str {
        self.identity.table_name()
    }
}

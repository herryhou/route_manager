//! rm-edit: manual-edit domain and application services. Zero SQL —
//! persistence lives in rm-db; this crate orchestrates and decides.

mod line;
mod rename;
mod station_move;
mod stop;
mod undo;

pub use line::*;
pub use rename::*;
pub use station_move::*;
pub use stop::*;
pub use undo::*;

/// The crate's public edit-error surface. Formerly defined by the rename
/// module; now a standalone shim type mirroring `rm_db::EditError` until
/// the final migration step flips rm-web over to the rm-db variants.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("new station name is empty")]
    EmptyNewName,
    #[error("new station name equals the current name")]
    NoOpRename,
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station {0} does not exist or is not current")]
    UnknownStation(i64),
    #[error("edit changes nothing")]
    EmptyEdit,
    #[error("edit sets no field to a new value")]
    NoOpEdit,
    #[error("route {0} does not exist")]
    UnknownRoute(String),
    #[error("invalid polyline: {0}")]
    InvalidLine(String),
    #[error("line {0}/{1} was edited by someone else — reload and retry")]
    LineNotCurrent(String, i64),
}

/// rm-db's internal edit errors surface 1:1 as this crate's public
/// EditError (stop-side semantics).
pub(crate) fn map_db_err(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<rm_db::EditError>() {
        Some(rm_db::EditError::NotCurrent(id)) => EditError::NotCurrent(*id).into(),
        Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
        Some(rm_db::EditError::UnknownStation(id)) => EditError::UnknownStation(*id).into(),
        Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
        Some(rm_db::EditError::UnknownRoute(x)) => EditError::UnknownRoute(x.clone()).into(),
        Some(rm_db::EditError::InvalidLine(m)) => EditError::InvalidLine(m.clone()).into(),
        Some(rm_db::EditError::LineNotCurrent(x, d)) => {
            EditError::LineNotCurrent(x.clone(), *d).into()
        }
        Some(rm_db::EditError::EmptyEdit) => EditError::EmptyEdit.into(),
        Some(rm_db::EditError::NoOpEdit) => EditError::NoOpEdit.into(),
        Some(rm_db::EditError::EmptyNewName) => EditError::EmptyNewName.into(),
        Some(rm_db::EditError::NoOpRename) => EditError::NoOpRename.into(),
        _ => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapped error's EditError variant, as Debug — None if it passed
    /// through untyped.
    fn variant(e: anyhow::Error) -> Option<String> {
        e.downcast_ref::<EditError>().map(|e| format!("{e:?}"))
    }

    #[test]
    fn map_db_err_maps_all_four_variants_one_to_one() {
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(
                rm_db::EditError::NotCurrent(1)
            ))),
            Some("NotCurrent(1)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(
                rm_db::EditError::StationNotCurrent(2)
            ))),
            Some("StationNotCurrent(2)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(
                rm_db::EditError::UnknownStation(3)
            ))),
            Some("UnknownStation(3)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(
                rm_db::EditError::NotUndoable(4)
            ))),
            Some("NotUndoable(4)".into())
        );
    }

    #[test]
    fn map_db_err_passes_unrelated_errors_through() {
        let raw = anyhow::anyhow!("boom");
        assert_eq!(map_db_err(raw).to_string(), "boom");
    }

    #[test]
    fn map_db_err_maps_rename_guards() {
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(
                rm_db::EditError::EmptyNewName
            ))),
            Some("EmptyNewName".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(rm_db::EditError::NoOpRename))),
            Some("NoOpRename".into())
        );
    }
}

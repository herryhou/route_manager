//! rm-edit: manual-edit domain and application services. Zero SQL —
//! persistence lives in rm-db; this crate orchestrates and decides.

mod rename;
mod station_move;
mod stop;
mod undo;

pub use rename::*;
pub use station_move::*;
pub use stop::*;
pub use undo::*;

/// rm-db's internal edit errors surface 1:1 as this crate's public
/// EditError (stop-side semantics).
pub(crate) fn map_db_err(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<rm_db::EditError>() {
        Some(rm_db::EditError::NotCurrent(id)) => EditError::NotCurrent(*id).into(),
        Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
        Some(rm_db::EditError::UnknownStation(id)) => EditError::UnknownStation(*id).into(),
        Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
        _ => e,
    }
}

/// Station-flavored mapping: a stale pole version (NotCurrent, fired by a
/// rename's per-pole guard) surfaces as the station-level error — rename
/// and move act on the station as a whole.
pub(crate) fn map_station_db_err(e: anyhow::Error) -> anyhow::Error {
    if let Some(rm_db::EditError::NotCurrent(id)) = e.downcast_ref::<rm_db::EditError>() {
        return EditError::StationNotCurrent(*id).into();
    }
    map_db_err(e)
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
            variant(map_db_err(anyhow::Error::new(rm_db::EditError::NotCurrent(1)))),
            Some("NotCurrent(1)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(rm_db::EditError::StationNotCurrent(2)))),
            Some("StationNotCurrent(2)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(rm_db::EditError::UnknownStation(3)))),
            Some("UnknownStation(3)".into())
        );
        assert_eq!(
            variant(map_db_err(anyhow::Error::new(rm_db::EditError::NotUndoable(4)))),
            Some("NotUndoable(4)".into())
        );
    }

    #[test]
    fn map_db_err_passes_unrelated_errors_through() {
        let raw = anyhow::anyhow!("boom");
        assert_eq!(map_db_err(raw).to_string(), "boom");
    }

    #[test]
    fn map_station_db_err_remaps_stale_pole_to_station_error() {
        // scenario-1 semantics: a stale pole surfaces as the station-level error
        assert_eq!(
            variant(map_station_db_err(anyhow::Error::new(rm_db::EditError::NotCurrent(10)))),
            Some("StationNotCurrent(10)".into())
        );
    }

    #[test]
    fn map_station_db_err_delegates_the_rest() {
        assert_eq!(
            variant(map_station_db_err(anyhow::Error::new(
                rm_db::EditError::StationNotCurrent(2)
            ))),
            Some("StationNotCurrent(2)".into())
        );
        assert_eq!(
            variant(map_station_db_err(anyhow::Error::new(rm_db::EditError::NotUndoable(4)))),
            Some("NotUndoable(4)".into())
        );
    }
}

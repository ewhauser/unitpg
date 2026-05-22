use crate::*;

const MAX_HOT_REDIRECT_HOPS: usize = 1_000_000;

#[derive(Debug, Default)]
pub(crate) struct StorageState {
    pub(crate) relations: HashMap<u32, RelationStorage>,
    pub(crate) epoch: u64,
    pub(crate) generation: u64,
}

impl StorageState {
    pub(crate) fn relation_mut(&mut self, relid: u32) -> &mut RelationStorage {
        self.relations.entry(relid).or_default()
    }

    pub(crate) fn begin_explicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            self.commit_implicit_transaction(session);
        }
        session.ensure_transaction();
        session.explicit_transaction = true;
    }

    pub(crate) fn commit_explicit_transaction(&mut self, session: &mut SessionStorage) {
        while !session.transaction_stack.is_empty() {
            self.commit_top_overlay(session);
        }
        session.explicit_transaction = false;
        self.generation = self.generation.saturating_add(1);
    }

    pub(crate) fn abort_explicit_transaction(&mut self, session: &mut SessionStorage) {
        self.rollback_all_overlays(session);
        session.explicit_transaction = false;
        self.epoch = self.epoch.saturating_add(1);
    }

    pub(crate) fn commit_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if session.explicit_transaction {
            return;
        }
        while !session.transaction_stack.is_empty() {
            self.commit_top_overlay(session);
        }
        self.generation = self.generation.saturating_add(1);
    }

    pub(crate) fn abort_implicit_transaction(&mut self, session: &mut SessionStorage) {
        if !session.explicit_transaction {
            self.rollback_all_overlays(session);
            self.epoch = self.epoch.saturating_add(1);
        }
    }

    pub(crate) fn rollback_all_overlays(&mut self, session: &mut SessionStorage) {
        while let Some(overlay) = session.transaction_stack.pop() {
            self.rollback_overlay_from_relations(overlay);
        }
    }

    pub(crate) fn commit_top_overlay(&mut self, session: &mut SessionStorage) {
        let Some(mut overlay) = session.transaction_stack.pop() else {
            return;
        };
        if let Some(parent) = session.transaction_stack.last_mut() {
            parent.append_from(&mut overlay);
            return;
        }
        self.commit_overlay_to_relations(overlay);
    }

    pub(crate) fn commit_overlay_to_relations(&mut self, overlay: TransactionOverlay) {
        for (relid, tids) in &overlay.inserted_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    relation.mark_live(*tid);
                }
            }
        }

        for (relid, tids) in &overlay.invalidated_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    relation.mark_dead(*tid);
                }
            }
        }

        for (relid, redirects) in overlay.hot_redirect_inserts {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.hot_redirects.extend(redirects);
            }
        }

        for (relid, keys) in overlay.primary_key_deletes {
            if let Some(relation) = self.relations.get_mut(&relid) {
                for key in keys {
                    relation.primary_key_index.remove(&key);
                }
            }
        }

        for (relid, entries) in overlay.primary_key_inserts {
            if let Some(relation) = self.relations.get_mut(&relid) {
                for (key, tid) in entries {
                    if relation.tuple_slice(tid, false).is_some() {
                        relation.primary_key_index.insert(key, tid);
                    }
                }
            }
        }
    }

    pub(crate) fn rollback_overlay_from_relations(&mut self, overlay: TransactionOverlay) {
        let has_new_pages = overlay.new_pages.values().any(|blocks| !blocks.is_empty());
        let has_page_rewinds = overlay
            .page_checkpoints
            .values()
            .any(|pages| !pages.is_empty());

        for (relid, blocks) in &overlay.new_pages {
            if let Some(relation) = self.relations.get_mut(relid) {
                for block in blocks {
                    relation.remove_page(*block);
                }
            }
        }

        for (relid, checkpoints) in &overlay.page_checkpoints {
            if let Some(relation) = self.relations.get_mut(relid) {
                for (block, checkpoint) in checkpoints {
                    if let Some(page) = relation.page_mut(*block) {
                        page.restore_to(checkpoint);
                    }
                }
            }
        }

        for (relid, checkpoint) in overlay.relation_checkpoints {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.restore_metadata(checkpoint);
            }
        }

        if has_page_rewinds {
            STORAGE2_ARENA_REWINDS.fetch_add(1, Ordering::Relaxed);
        }
        if has_new_pages {
            STORAGE2_ARENA_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn clear_relation(&mut self, session: &mut SessionStorage, relid: u32) {
        if session.transaction_stack.is_empty() {
            self.relations.insert(relid, RelationStorage::default());
            return;
        }

        let visible_tids = self.visible_tids(session, relid);
        let primary_keys = primary_index_spec_for_relation_oid(Oid(relid))
            .map(|index_spec| {
                visible_tids
                    .iter()
                    .filter_map(|tid| {
                        self.find_visible_tuple(session, relid, *tid)
                            .and_then(|tuple| index_key_for_decoded(&index_spec, &tuple.values))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction stack was checked");
        for tid in visible_tids {
            overlay.invalidate(relid, tid);
        }
        for key in primary_keys {
            overlay.delete_primary_key(relid, key);
        }
        session.mark_scans_visibility_delta(relid);
    }

    pub(crate) fn append_pending_tuple(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tuple: &[u8],
    ) -> Result<Tid, CatalogError> {
        session.ensure_transaction();
        let epoch = self.epoch;
        let generation = self.generation;
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        let relation = self.relation_mut(relid);
        overlay.checkpoint_relation(relid, relation);

        let before_next_block = relation.next_block;
        let block = relation
            .append_target_block(tuple.len(), epoch, generation)
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        if block >= before_next_block {
            overlay.record_new_page(relid, block);
        } else if let Some(page) = relation.page(block) {
            overlay.checkpoint_page(relid, page);
        }

        let tid = relation
            .append_pending_tuple(block, tuple)
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        overlay.insert_tid(relid, tid);
        Ok(tid)
    }

    pub(crate) fn find_visible_tuple<'a>(
        &'a self,
        session: &'a SessionStorage,
        relid: u32,
        tid: Tid,
    ) -> Option<DecodedTuple<'a>> {
        self.find_visible_tuple_in_overlays(&session.transaction_stack, relid, tid)
    }

    pub(crate) fn find_visible_tuple_in_overlays<'a>(
        &'a self,
        overlays: &[TransactionOverlay],
        relid: u32,
        tid: Tid,
    ) -> Option<DecodedTuple<'a>> {
        let tid = self.resolve_tid_redirect_in_overlays(overlays, relid, tid);
        decode_tuple(
            tid,
            self.visible_tuple_slice_in_overlays(overlays, relid, tid)?,
        )
    }

    pub(crate) fn resolve_tid_redirect_in_overlays(
        &self,
        overlays: &[TransactionOverlay],
        relid: u32,
        mut tid: Tid,
    ) -> Tid {
        for _ in 0..MAX_HOT_REDIRECT_HOPS {
            if let Some(next_tid) = overlay_tid_redirect(overlays, relid, tid) {
                tid = next_tid;
                continue;
            }
            if let Some(next_tid) = self
                .relations
                .get(&relid)
                .and_then(|relation| relation.hot_redirects.get(&tid))
                .copied()
            {
                tid = next_tid;
                continue;
            }
            break;
        }
        tid
    }

    pub(crate) fn visible_tuple_slice_in_overlays<'a>(
        &'a self,
        overlays: &[TransactionOverlay],
        relid: u32,
        tid: Tid,
    ) -> Option<&'a [u8]> {
        let tid = self.resolve_tid_redirect_in_overlays(overlays, relid, tid);
        if overlays_invalidate_tid(overlays, relid, tid) {
            return None;
        }
        self.relations
            .get(&relid)?
            .tuple_slice(tid, overlays_own_inserted_tid(overlays, relid, tid))
    }

    pub(crate) fn visible_tids(&self, session: &SessionStorage, relid: u32) -> Vec<Tid> {
        let mut tids = Vec::new();
        if let Some(relation) = self.relations.get(&relid) {
            tids.extend(relation.live_tids());
        }
        for overlay in &session.transaction_stack {
            if let Some(inserted) = overlay.inserted_tids.get(&relid) {
                tids.extend(inserted.iter().copied());
            }
        }
        tids.sort_unstable();
        tids.dedup();
        tids.retain(|tid| self.find_visible_tuple(session, relid, *tid).is_some());
        tids
    }

    pub(crate) fn visible_row_count(&self, session: &SessionStorage, relid: u32) -> usize {
        let committed = self
            .relations
            .get(&relid)
            .map(|relation| relation.live_tuple_count)
            .unwrap_or_default();
        committed
            .saturating_add(session.transaction_visible_insert_count(relid))
            .saturating_sub(session.transaction_invalidated_live_count(relid))
    }

    pub(crate) fn next_visible_tuple_slice_in_overlays<'a>(
        &'a self,
        overlays: &[TransactionOverlay],
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
    ) -> Option<(Tid, &'a [u8])> {
        let relation = self.relations.get(&relid)?;
        if forward {
            let mut block = cursor.block;
            while usize::try_from(block).ok()? < high_water_offsets.len() {
                let max_offset = high_water_offsets[block as usize];
                if relation
                    .pages
                    .get(block as usize)
                    .and_then(Option::as_ref)
                    .is_none()
                {
                    block = block.checked_add(1)?;
                    continue;
                }
                let mut offset = if block == cursor.block {
                    cursor.offset
                } else {
                    1
                };
                while offset <= max_offset {
                    let tid = Tid { block, offset };
                    if let Some(tuple) = self.visible_tuple_slice_in_overlays(overlays, relid, tid)
                    {
                        return Some((tid, tuple));
                    }
                    offset = offset.checked_add(1)?;
                }
                block = block.checked_add(1)?;
            }
            return None;
        }

        let mut block = if cursor.block == u32::MAX {
            high_water_offsets.len().checked_sub(1)?.try_into().ok()?
        } else {
            cursor.block
        };
        loop {
            let max_offset = high_water_offsets.get(block as usize).copied()?;
            if relation
                .pages
                .get(block as usize)
                .and_then(Option::as_ref)
                .is_some()
            {
                let mut offset = if block == cursor.block && cursor.offset != u16::MAX {
                    cursor.offset.min(max_offset)
                } else {
                    max_offset
                };
                while offset > 0 {
                    let tid = Tid { block, offset };
                    if let Some(tuple) = self.visible_tuple_slice_in_overlays(overlays, relid, tid)
                    {
                        return Some((tid, tuple));
                    }
                    offset -= 1;
                }
            }
            if block == 0 {
                return None;
            }
            block -= 1;
        }
    }

    pub(crate) fn next_committed_tuple_slice<'a>(
        &'a self,
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
    ) -> Option<(Tid, &'a [u8])> {
        let relation = self.relations.get(&relid)?;
        if forward {
            let mut block = cursor.block;
            while usize::try_from(block).ok()? < high_water_offsets.len() {
                let max_offset = high_water_offsets[block as usize];
                if relation
                    .pages
                    .get(block as usize)
                    .and_then(Option::as_ref)
                    .is_none()
                {
                    block = block.checked_add(1)?;
                    continue;
                }
                let mut offset = if block == cursor.block {
                    cursor.offset
                } else {
                    1
                };
                while offset <= max_offset {
                    let tid = Tid { block, offset };
                    if let Some(tuple) = relation.tuple_slice(tid, false) {
                        return Some((tid, tuple));
                    }
                    offset = offset.checked_add(1)?;
                }
                block = block.checked_add(1)?;
            }
            return None;
        }

        let mut block = if cursor.block == u32::MAX {
            high_water_offsets.len().checked_sub(1)?.try_into().ok()?
        } else {
            cursor.block
        };
        loop {
            let max_offset = high_water_offsets.get(block as usize).copied()?;
            if relation
                .pages
                .get(block as usize)
                .and_then(Option::as_ref)
                .is_some()
            {
                let mut offset = if block == cursor.block && cursor.offset != u16::MAX {
                    cursor.offset.min(max_offset)
                } else {
                    max_offset
                };
                while offset > 0 {
                    let tid = Tid { block, offset };
                    if let Some(tuple) = relation.tuple_slice(tid, false) {
                        return Some((tid, tuple));
                    }
                    offset -= 1;
                }
            }
            if block == 0 {
                return None;
            }
            block -= 1;
        }
    }

    pub(crate) fn primary_key_lookup(
        &self,
        session: &SessionStorage,
        relid: u32,
        key: &IndexKey,
    ) -> Option<Tid> {
        for overlay in session.transaction_stack.iter().rev() {
            if let Some(tid) = overlay
                .primary_key_inserts
                .get(&relid)
                .and_then(|entries| entries.get(key))
                .copied()
                && self.find_visible_tuple(session, relid, tid).is_some()
            {
                return Some(tid);
            }
            if overlay
                .primary_key_deletes
                .get(&relid)
                .is_some_and(|keys| keys.contains(key))
            {
                return None;
            }
        }
        let tid = self
            .relations
            .get(&relid)?
            .primary_key_index
            .get(key)
            .copied()?;
        let tid = self.resolve_tid_redirect_in_overlays(&session.transaction_stack, relid, tid);
        self.find_visible_tuple(session, relid, tid).map(|_| tid)
    }

    pub(crate) fn find_visible_by_index_key_excluding(
        &self,
        session: &SessionStorage,
        relid: u32,
        index_spec: &UniqueIndexSpec,
        key: &IndexKey,
        replacing_tid: Option<Tid>,
    ) -> Option<Tid> {
        let replacing_tid = replacing_tid.map(|tid| {
            self.resolve_tid_redirect_in_overlays(&session.transaction_stack, relid, tid)
        });
        if index_spec.is_primary {
            if let Some(tid) = self.primary_key_lookup(session, relid, key)
                && Some(tid) != replacing_tid
            {
                return Some(tid);
            }
            return None;
        }

        self.visible_tids(session, relid).into_iter().find(|tid| {
            Some(*tid) != replacing_tid
                && self
                    .find_visible_tuple(session, relid, *tid)
                    .and_then(|tuple| index_key_for_decoded(index_spec, &tuple.values))
                    .as_ref()
                    == Some(key)
        })
    }

    pub(crate) fn unique_index_conflict_for_input(
        &self,
        session: &SessionStorage,
        relid: u32,
        input: &RowInput<'_>,
        replacing_tid: Option<Tid>,
    ) -> Option<Oid> {
        for index_spec in unique_index_specs_for_relation_oid(Oid(relid)) {
            let Some(key) = index_key_for_input(&index_spec, input) else {
                continue;
            };
            if self
                .find_visible_by_index_key_excluding(
                    session,
                    relid,
                    &index_spec,
                    &key,
                    replacing_tid,
                )
                .is_some()
            {
                return Some(index_spec.index_oid);
            }
        }
        None
    }

    pub(crate) fn metrics(&self, session: &SessionStorage) -> FastPgStorage2Metrics {
        FastPgStorage2Metrics {
            committed_page_bytes: self
                .relations
                .values()
                .map(RelationStorage::accounted_bytes)
                .sum(),
            transaction_page_bytes: session.transaction_bytes(),
            scan_scratch_bytes: session.scan_bytes(),
            live_tuple_bytes: self
                .relations
                .values()
                .map(RelationStorage::live_tuple_bytes)
                .sum::<usize>()
                + session.transaction_live_tuple_bytes(),
            dead_tuple_bytes: self
                .relations
                .values()
                .map(RelationStorage::dead_tuple_bytes)
                .sum::<usize>()
                + session.transaction_dead_tuple_bytes(),
            index_bytes: self
                .relations
                .values()
                .map(RelationStorage::index_bytes)
                .sum::<usize>()
                + session.transaction_index_bytes(),
            page_count: self
                .relations
                .values()
                .map(RelationStorage::page_count)
                .sum::<usize>(),
            arena_rewinds: STORAGE2_ARENA_REWINDS.load(Ordering::Relaxed),
            arena_drops: STORAGE2_ARENA_DROPS.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum UniqueCheck {
    Enforce,
    Skip,
}

static STORAGE: OnceLock<Mutex<StorageState>> = OnceLock::new();

pub(crate) fn storage() -> &'static Mutex<StorageState> {
    STORAGE.get_or_init(|| Mutex::new(StorageState::default()))
}

pub(crate) fn with_storage<R>(f: impl FnOnce(&mut StorageState, &mut SessionStorage) -> R) -> R {
    let session = current_session_storage();
    let mut session = match session.lock() {
        Ok(session) => session,
        Err(poisoned) => poisoned.into_inner(),
    };
    match storage().lock() {
        Ok(mut state) => f(&mut state, &mut session),
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            f(&mut state, &mut session)
        }
    }
}

pub(crate) fn relation_insert_impl(
    relid: u32,
    input: RowInput<'_>,
    tid_out: *mut u64,
    unique_check: UniqueCheck,
) -> bool {
    let result = with_storage(|state, session| -> Result<Option<Tid>, CatalogError> {
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict_for_input(session, relid, &input, None)
                .is_some()
        {
            return Ok(None);
        }

        let tuple = build_tuple(&input)?;
        let tid = state.append_pending_tuple(session, relid, &tuple)?;

        if let Some(index_spec) = primary_index_spec_for_relation_oid(Oid(relid))
            && let Some(key) = index_key_for_input(&index_spec, &input)
        {
            session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .insert_primary_key(relid, key, tid);
        }
        Ok(Some(tid))
    });

    match result {
        Ok(Some(tid)) => {
            if !tid_out.is_null() {
                unsafe {
                    *tid_out = tid.pack();
                }
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

pub(crate) fn relation_update_impl(
    relid: u32,
    packed_tid: u64,
    input: RowInput<'_>,
    new_tid_out: *mut u64,
    unique_check: UniqueCheck,
    record_hot_redirect: bool,
) -> bool {
    let Some(old_tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let result = with_storage(|state, session| -> Result<Option<Tid>, CatalogError> {
        let old_tid =
            state.resolve_tid_redirect_in_overlays(&session.transaction_stack, relid, old_tid);
        let Some(old_tuple) = state.find_visible_tuple(session, relid, old_tid) else {
            return Ok(None);
        };
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict_for_input(session, relid, &input, Some(old_tid))
                .is_some()
        {
            return Ok(None);
        }
        let old_primary_key = primary_index_spec_for_relation_oid(Oid(relid))
            .and_then(|index_spec| index_key_for_decoded(&index_spec, &old_tuple.values));
        drop(old_tuple);

        let tuple = build_tuple(&input)?;
        let new_tid = state.append_pending_tuple(session, relid, &tuple)?;
        let new_primary_key = primary_index_spec_for_relation_oid(Oid(relid))
            .and_then(|index_spec| index_key_for_input(&index_spec, &input));

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay.invalidate(relid, old_tid);
        if record_hot_redirect {
            overlay.insert_hot_redirect(relid, old_tid, new_tid);
        }
        if old_primary_key.is_some()
            && old_primary_key != new_primary_key
            && let Some(key) = old_primary_key
        {
            overlay.delete_primary_key(relid, key);
        }
        if let Some(key) = new_primary_key {
            overlay.insert_primary_key(relid, key, new_tid);
        }
        session.mark_scans_visibility_delta(relid);
        Ok(Some(new_tid))
    });

    match result {
        Ok(Some(tid)) => {
            if !new_tid_out.is_null() {
                unsafe {
                    *new_tid_out = tid.pack();
                }
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            set_last_storage_error(error);
            false
        }
    }
}

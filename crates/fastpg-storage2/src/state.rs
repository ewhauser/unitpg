use crate::*;
use smallvec::SmallVec;

const MAX_HOT_REDIRECT_HOPS: usize = 1_000_000;

#[derive(Debug, Default)]
pub(crate) struct StorageState {
    pub(crate) relations: HashMap<u32, RelationStorage>,
    pub(crate) epoch: u64,
    pub(crate) generation: u64,
}

fn add_row_count_delta(deltas: &mut SmallVec<[(u32, isize); 4]>, relid: u32, delta: isize) {
    if let Some((_, existing)) = deltas
        .iter_mut()
        .find(|(entry_relid, _)| *entry_relid == relid)
    {
        *existing += delta;
        return;
    }
    deltas.push((relid, delta));
}

impl StorageState {
    pub(crate) fn relation_mut(&mut self, relid: u32) -> &mut RelationStorage {
        self.relations.entry(relid).or_default()
    }

    fn refresh_cached_row_count(&self, relid: u32) {
        let committed = self
            .relations
            .get(&relid)
            .map(|relation| relation.live_tuple_count)
            .unwrap_or_default();
        store_committed_row_count(relid, committed);
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
        let mut overlay = overlay;
        let has_cleared_relations = !overlay.cleared_relations.is_empty();
        let mut row_count_deltas = SmallVec::<[(u32, isize); 4]>::new();

        let pending_pages = overlay.pending_pages.drain().collect::<Vec<_>>();
        let epoch = self.epoch;
        let generation = self.generation;
        for (relid, pages) in pending_pages {
            let relation = self.relation_mut(relid);
            let mut remaps = Vec::new();
            for page in pages {
                relation.pending_reserved_blocks.remove(&page.block);
                if overlay.pending_page_needs_stable_tid(relid, page.block)
                    || relation.live_tuple_count == 0
                        && relation.pending_tuple_count == 0
                        && relation.dead_tuple_count == 0
                        && relation.pages.iter().all(Option::is_none)
                {
                    for (index, _) in page.line_pointers.iter().enumerate() {
                        if let Ok(offset) = u16::try_from(index + 1) {
                            relation.pending_reserved_tids.remove(&Tid {
                                block: page.block,
                                offset,
                            });
                        }
                    }
                    relation.insert_page(page);
                    continue;
                }
                for (index, line) in page.line_pointers.iter().enumerate() {
                    if line.state == LinePointerState::Dead {
                        continue;
                    }
                    let Some(offset) = u16::try_from(index + 1).ok() else {
                        continue;
                    };
                    let old_tid = Tid {
                        block: page.block,
                        offset,
                    };
                    let Some(tuple) = page.tuple_slice(old_tid.offset, true) else {
                        continue;
                    };
                    relation.pending_reserved_tids.remove(&old_tid);
                    let block = relation
                        .append_target_block(tuple.len(), epoch, generation)
                        .expect("reserved pending tuple should fit into committed relation");
                    let new_tid = relation
                        .append_pending_tuple(block, tuple)
                        .expect("reserved pending tuple should append into committed relation");
                    relation.set_insert_metadata(new_tid, line.xmin, line.cmin);
                    relation.set_row_xmax(new_tid, line.xmax);
                    if old_tid != new_tid {
                        relation.hot_redirects.insert(old_tid, new_tid);
                        relation.update_redirects.insert(old_tid, new_tid);
                    }
                    remaps.push((old_tid, new_tid));
                }
            }
            overlay.remap_tids(relid, &remaps);
        }

        for (relid, tids) in &overlay.inserted_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    if relation.mark_live(*tid) {
                        add_row_count_delta(&mut row_count_deltas, *relid, 1);
                    }
                }
            }
        }

        for (relid, tids) in &overlay.invalidated_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                let metadata = overlay.invalidated_metadata.get(relid);
                for tid in tids {
                    if has_cleared_relations
                        && overlay.clear_insert_shadows_invalidation(*relid, *tid)
                    {
                        continue;
                    }
                    if relation.mark_dead(*tid) {
                        add_row_count_delta(&mut row_count_deltas, *relid, -1);
                    }
                    if let Some(metadata) = metadata.and_then(|entries| {
                        entries
                            .iter()
                            .find(|(entry_tid, _)| entry_tid == tid)
                            .map(|(_, metadata)| metadata)
                    }) {
                        relation.row_delete_xids.insert(*tid, metadata.xid);
                        relation.row_delete_cids.insert(*tid, metadata.cid);
                    }
                }
            }
        }

        for (relid, redirects) in overlay.hot_redirect_inserts {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.hot_redirects.extend(redirects);
            }
        }

        for (relid, redirects) in overlay.update_redirect_inserts {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.update_redirects.extend(redirects);
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

        for (relid, delta) in row_count_deltas {
            if delta != 0 {
                self.refresh_cached_row_count(relid);
            }
        }
    }

    pub(crate) fn rollback_overlay_from_relations(&mut self, overlay: TransactionOverlay) {
        let has_inserted_tids = overlay.inserted_tids.values().any(|tids| !tids.is_empty());
        let cleared_relids = overlay
            .cleared_relations
            .keys()
            .copied()
            .collect::<Vec<_>>();

        for (relid, tids) in &overlay.inserted_tids {
            if let Some(relation) = self.relations.get_mut(relid) {
                for tid in tids {
                    relation.pending_reserved_tids.remove(tid);
                    relation.mark_dead(*tid);
                }
            }
        }
        for (relid, pages) in &overlay.pending_pages {
            if let Some(relation) = self.relations.get_mut(relid) {
                for page in pages {
                    relation.pending_reserved_blocks.remove(&page.block);
                }
            }
        }

        for (relid, checkpoint) in overlay.relation_checkpoints {
            if let Some(relation) = self.relations.get_mut(&relid) {
                relation.restore_rollback_metadata_preserving_tid_space(checkpoint);
                self.refresh_cached_row_count(relid);
            }
        }

        for (relid, relation) in overlay.cleared_relations {
            self.relations.insert(relid, relation);
            self.refresh_cached_row_count(relid);
        }

        for relid in cleared_relids {
            self.refresh_cached_row_count(relid);
        }

        if has_inserted_tids {
            STORAGE2_ARENA_REWINDS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn clear_relation(&mut self, session: &mut SessionStorage, relid: u32) {
        self.clear_relation_impl(session, relid, false);
    }

    fn clear_relation_preserving_tid_space(&mut self, session: &mut SessionStorage, relid: u32) {
        self.clear_relation_impl(session, relid, true);
    }

    fn clear_relation_impl(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        preserve_tid_space: bool,
    ) {
        if session.transaction_stack.is_empty() {
            let old_relation = self.relations.remove(&relid).unwrap_or_default();
            let mut replacement = RelationStorage {
                max_tuples_per_block: old_relation.max_tuples_per_block,
                ..RelationStorage::default()
            };
            if preserve_tid_space {
                replacement.next_block = old_relation.next_block;
            }
            self.relations.insert(relid, replacement);
            self.refresh_cached_row_count(relid);
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
        let old_relation = self.relations.remove(&relid).unwrap_or_default();
        let mut replacement = RelationStorage {
            max_tuples_per_block: old_relation.max_tuples_per_block,
            ..RelationStorage::default()
        };
        if preserve_tid_space {
            replacement.next_block = old_relation.next_block;
        }
        overlay.record_relation_clear(relid, old_relation);
        for tid in visible_tids {
            overlay.invalidate(relid, tid);
        }
        for key in primary_keys {
            overlay.delete_primary_key(relid, key);
        }
        self.relations.insert(relid, replacement);
        session.mark_scans_visibility_delta(relid);
    }

    pub(crate) fn replace_relation_from(
        &mut self,
        session: &mut SessionStorage,
        dst_relid: u32,
        src_relid: u32,
    ) -> Result<(), CatalogError> {
        let tuples = self
            .visible_tids(session, src_relid)
            .into_iter()
            .filter_map(|tid| {
                self.visible_tuple_slice_in_overlays(&session.transaction_stack, src_relid, tid)
                    .map(Vec::from)
            })
            .collect::<Vec<_>>();
        let primary_index_spec = primary_index_spec_for_relation_oid(Oid(dst_relid));

        self.clear_relation_preserving_tid_space(session, dst_relid);

        for tuple in tuples {
            let tid = self.append_pending_tuple(session, dst_relid, &tuple)?;
            if let Some(index_spec) = primary_index_spec.as_ref()
                && let Some(decoded) = decode_tuple(tid, &tuple)
                && let Some(key) = index_key_for_decoded(index_spec, &decoded.values)
            {
                session
                    .transaction_stack
                    .last_mut()
                    .expect("transaction was just ensured")
                    .insert_primary_key(dst_relid, key, tid);
            }
        }

        session.mark_scans_visibility_delta(dst_relid);
        Ok(())
    }

    fn reserve_pending_page(&mut self, relid: u32, tuple_len: usize) -> Result<Page, CatalogError> {
        let epoch = self.epoch;
        let generation = self.generation;
        let relation = self.relation_mut(relid);
        let block = relation
            .reserve_block()
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        relation.pending_reserved_blocks.insert(block);
        Ok(Page::new(block, epoch, generation, tuple_len))
    }

    pub(crate) fn append_pending_tuple(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tuple: &[u8],
    ) -> Result<Tid, CatalogError> {
        session.ensure_transaction();
        if let Some(tid) = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .append_pending_tuple_to_existing_page(relid, tuple)
        {
            session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .insert_tid(relid, tid);
            return Ok(tid);
        }

        let page = self.reserve_pending_page(relid, tuple.len())?;
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        let tid = overlay
            .append_pending_tuple_to_new_page(relid, page, tuple)
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        overlay.insert_tid(relid, tid);
        Ok(tid)
    }

    pub(crate) fn append_pending_input_tuple(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        input: &RowInput<'_>,
    ) -> Result<Tid, CatalogError> {
        let tuple_len = tuple_storage_len(input)?;
        self.append_pending_input_tuple_with_len(session, relid, input, tuple_len)
    }

    pub(crate) fn append_pending_input_tuple_with_len(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        input: &RowInput<'_>,
        tuple_len: usize,
    ) -> Result<Tid, CatalogError> {
        session.ensure_transaction();
        if let Some(tid) = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured")
            .append_pending_input_tuple_to_existing_page(relid, input, tuple_len)?
        {
            session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .insert_tid(relid, tid);
            return Ok(tid);
        }

        let page = self.reserve_pending_page(relid, tuple_len)?;
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        let tid = overlay
            .append_pending_input_tuple_to_new_page(relid, page, input, tuple_len)?
            .ok_or_else(|| storage_limit_error("storage2 could not allocate tuple page"))?;
        overlay.insert_tid(relid, tid);
        Ok(tid)
    }

    fn set_insert_metadata(&mut self, relid: u32, tid: Tid, xid: u32, cid: u32) {
        if let Some(relation) = self.relations.get_mut(&relid) {
            relation.set_insert_metadata(tid, xid, cid);
        }
    }

    fn set_row_xmax(&mut self, relid: u32, tid: Tid, xmax: u32) {
        if let Some(relation) = self.relations.get_mut(&relid) {
            relation.set_row_xmax(tid, xmax);
        }
    }

    pub(crate) fn record_insert_metadata(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tid: Tid,
        xid: u32,
        cid: u32,
    ) {
        if let Some(overlay) = session.transaction_stack.last_mut()
            && overlay
                .inserted_tids
                .get(&relid)
                .is_some_and(|tids| tids.contains(&tid))
        {
            overlay.set_insert_cid(relid, tid, cid);
            if !overlay.set_pending_insert_metadata(relid, tid, xid, cid) {
                self.set_insert_metadata(relid, tid, xid, cid);
            }
        }
    }

    pub(crate) fn record_invalidate_metadata(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tid: Tid,
        xid: u32,
        cid: u32,
    ) {
        if let Some(overlay) = session.transaction_stack.last_mut()
            && overlay
                .invalidated_tids
                .get(&relid)
                .is_some_and(|tids| tids.contains(&tid))
        {
            overlay.set_invalidate_metadata(relid, tid, xid, cid);
        }
    }

    pub(crate) fn record_row_xmax(
        &mut self,
        session: &mut SessionStorage,
        relid: u32,
        tid: Tid,
        xmax: u32,
    ) {
        if let Some(overlay) = session.transaction_stack.last_mut()
            && overlay
                .inserted_tids
                .get(&relid)
                .is_some_and(|tids| tids.contains(&tid))
            && !overlay.set_pending_row_xmax(relid, tid, xmax)
        {
            self.set_row_xmax(relid, tid, xmax);
        }
    }

    pub(crate) fn row_xmin(&self, session: &SessionStorage, relid: u32, tid: Tid) -> u32 {
        if let Some(xmin) = overlay_pending_row_xmin(&session.transaction_stack, relid, tid) {
            return xmin;
        }
        self.relations
            .get(&relid)
            .and_then(|relation| relation.row_xmin(tid))
            .unwrap_or_default()
    }

    pub(crate) fn row_cmin(&self, session: &SessionStorage, relid: u32, tid: Tid) -> u32 {
        for overlay in session.transaction_stack.iter().rev() {
            if let Some(cid) = overlay.inserted_cids.get(&relid).and_then(|entries| {
                entries
                    .iter()
                    .find(|(entry_tid, _)| *entry_tid == tid)
                    .map(|(_, cid)| cid)
            }) {
                return *cid;
            }
        }
        self.relations
            .get(&relid)
            .and_then(|relation| relation.row_cmin(tid))
            .unwrap_or_default()
    }

    pub(crate) fn row_xmax(&self, session: &SessionStorage, relid: u32, tid: Tid) -> u32 {
        if let Some(xmax) = overlay_pending_row_xmax(&session.transaction_stack, relid, tid) {
            return xmax;
        }
        self.relations
            .get(&relid)
            .and_then(|relation| relation.row_xmax(tid))
            .unwrap_or_default()
    }

    pub(crate) fn row_delete_xid(&self, session: &SessionStorage, relid: u32, tid: Tid) -> u32 {
        for overlay in session.transaction_stack.iter().rev() {
            if let Some(xid) = overlay
                .invalidated_metadata
                .get(&relid)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|(entry_tid, _)| *entry_tid == tid)
                        .map(|(_, metadata)| metadata)
                })
                .map(|metadata| metadata.xid)
            {
                return xid;
            }
        }
        self.relations
            .get(&relid)
            .and_then(|relation| relation.row_delete_xids.get(&tid))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn row_delete_cid(&self, session: &SessionStorage, relid: u32, tid: Tid) -> u32 {
        for overlay in session.transaction_stack.iter().rev() {
            if let Some(cid) = overlay
                .invalidated_metadata
                .get(&relid)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|(entry_tid, _)| *entry_tid == tid)
                        .map(|(_, metadata)| metadata)
                })
                .map(|metadata| metadata.cid)
            {
                return cid;
            }
        }
        self.relations
            .get(&relid)
            .and_then(|relation| relation.row_delete_cids.get(&tid))
            .copied()
            .unwrap_or_default()
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
        overlays: &'a [TransactionOverlay],
        relid: u32,
        tid: Tid,
    ) -> Option<DecodedTuple<'a>> {
        let tid = self.resolve_tid_redirect_in_overlays(overlays, relid, tid);
        decode_tuple(
            tid,
            self.visible_tuple_slice_in_overlays(overlays, relid, tid)?,
        )
    }

    pub(crate) fn index_tid_all_dead(
        &self,
        session: &SessionStorage,
        relid: u32,
        tid: Tid,
    ) -> bool {
        let tid = self.resolve_tid_redirect_in_overlays(&session.transaction_stack, relid, tid);
        if self
            .physical_visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
            .is_some()
        {
            return false;
        }

        if let Some(state) =
            overlay_pending_line_pointer_state(&session.transaction_stack, relid, tid)
        {
            return state == LinePointerState::Dead;
        }

        self.relations.get(&relid).is_none_or(|relation| {
            !relation.pending_reserved_tids.contains(&tid)
                && !relation.pending_reserved_blocks.contains(&tid.block)
                && relation
                    .line_pointer_state(tid)
                    .is_none_or(|state| state == LinePointerState::Dead)
        })
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

    pub(crate) fn resolve_tid_redirect_in_overlays_compress(
        &mut self,
        overlays: &[TransactionOverlay],
        relid: u32,
        mut tid: Tid,
    ) -> Tid {
        let original_tid = tid;
        let mut first_committed_tid = None;
        let mut rest_committed_tids = Vec::new();
        let mut followed_overlay_redirect = false;

        for _ in 0..MAX_HOT_REDIRECT_HOPS {
            if let Some(next_tid) = overlay_tid_redirect(overlays, relid, tid) {
                followed_overlay_redirect = true;
                tid = next_tid;
                continue;
            }
            if let Some(next_tid) = self
                .relations
                .get(&relid)
                .and_then(|relation| relation.hot_redirects.get(&tid))
                .copied()
            {
                if !followed_overlay_redirect {
                    if first_committed_tid.is_some() {
                        rest_committed_tids.push(tid);
                    } else {
                        first_committed_tid = Some(tid);
                    }
                }
                tid = next_tid;
                continue;
            }
            break;
        }

        if !followed_overlay_redirect
            && tid != original_tid
            && let Some(relation) = self.relations.get_mut(&relid)
        {
            if let Some(first_tid) = first_committed_tid
                && first_tid != tid
            {
                relation.hot_redirects.insert(first_tid, tid);
            }
            for visited_tid in rest_committed_tids {
                if visited_tid != tid {
                    relation.hot_redirects.insert(visited_tid, tid);
                }
            }
        }

        tid
    }

    pub(crate) fn resolve_update_redirect_in_overlays_compress(
        &mut self,
        overlays: &[TransactionOverlay],
        relid: u32,
        mut tid: Tid,
    ) -> Tid {
        let original_tid = tid;
        let mut first_committed_tid = None;
        let mut rest_committed_tids = Vec::new();
        let mut followed_overlay_redirect = false;

        for _ in 0..MAX_HOT_REDIRECT_HOPS {
            if let Some(next_tid) = overlay_update_redirect(overlays, relid, tid) {
                followed_overlay_redirect = true;
                tid = next_tid;
                continue;
            }
            if let Some(next_tid) = self
                .relations
                .get(&relid)
                .and_then(|relation| relation.update_redirects.get(&tid))
                .copied()
            {
                if !followed_overlay_redirect {
                    if first_committed_tid.is_some() {
                        rest_committed_tids.push(tid);
                    } else {
                        first_committed_tid = Some(tid);
                    }
                }
                tid = next_tid;
                continue;
            }
            break;
        }

        if !followed_overlay_redirect
            && tid != original_tid
            && let Some(relation) = self.relations.get_mut(&relid)
        {
            if let Some(first_tid) = first_committed_tid
                && first_tid != tid
            {
                relation.update_redirects.insert(first_tid, tid);
            }
            for visited_tid in rest_committed_tids {
                if visited_tid != tid {
                    relation.update_redirects.insert(visited_tid, tid);
                }
            }
        }

        tid
    }

    pub(crate) fn resolve_update_redirect_in_overlays(
        &self,
        overlays: &[TransactionOverlay],
        relid: u32,
        mut tid: Tid,
    ) -> Tid {
        for _ in 0..MAX_HOT_REDIRECT_HOPS {
            if let Some(next_tid) = overlay_update_redirect(overlays, relid, tid) {
                tid = next_tid;
                continue;
            }
            if let Some(next_tid) = self
                .relations
                .get(&relid)
                .and_then(|relation| relation.update_redirects.get(&tid))
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
        overlays: &'a [TransactionOverlay],
        relid: u32,
        tid: Tid,
    ) -> Option<&'a [u8]> {
        let tid = self.resolve_tid_redirect_in_overlays(overlays, relid, tid);
        if let [overlay] = overlays {
            let include_pending = single_overlay_tid_visibility(overlay, relid, tid, None)?;
            if include_pending && let Some(tuple) = overlay.pending_tuple_slice(relid, tid) {
                return Some(tuple);
            }
            return self
                .relations
                .get(&relid)?
                .tuple_slice(tid, include_pending);
        }
        if overlays_invalidate_tid(overlays, relid, tid) {
            return None;
        }
        if overlays_clear_tid(overlays, relid, tid) {
            return None;
        }
        if overlays_own_inserted_tid(overlays, relid, tid)
            && let Some(tuple) = overlay_pending_tuple_slice(overlays, relid, tid)
        {
            return Some(tuple);
        }
        self.relations
            .get(&relid)?
            .tuple_slice(tid, overlays_own_inserted_tid(overlays, relid, tid))
    }

    pub(crate) fn physical_visible_tuple_slice_in_overlays<'a>(
        &'a self,
        overlays: &'a [TransactionOverlay],
        relid: u32,
        tid: Tid,
    ) -> Option<&'a [u8]> {
        if let [overlay] = overlays {
            let include_pending = single_overlay_tid_visibility(overlay, relid, tid, None)?;
            if include_pending && let Some(tuple) = overlay.pending_tuple_slice(relid, tid) {
                return Some(tuple);
            }
            return self
                .relations
                .get(&relid)?
                .tuple_slice(tid, include_pending);
        }
        if overlays_invalidate_tid(overlays, relid, tid) {
            return None;
        }
        if overlays_clear_tid(overlays, relid, tid) {
            return None;
        }
        if overlays_own_inserted_tid(overlays, relid, tid)
            && let Some(tuple) = overlay_pending_tuple_slice(overlays, relid, tid)
        {
            return Some(tuple);
        }
        self.relations
            .get(&relid)?
            .tuple_slice(tid, overlays_own_inserted_tid(overlays, relid, tid))
    }

    fn physical_visible_tuple_slice_for_session<'a>(
        &'a self,
        session: &'a SessionStorage,
        relid: u32,
        tid: Tid,
        has_visibility_deltas: bool,
    ) -> Option<&'a [u8]> {
        if !has_visibility_deltas {
            return self.relations.get(&relid)?.tuple_slice(tid, false);
        }
        self.physical_visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
    }

    pub(crate) fn visible_tuple_slice_in_overlays_at_cid<'a>(
        &'a self,
        overlays: &'a [TransactionOverlay],
        relid: u32,
        tid: Tid,
        curcid: u32,
    ) -> Option<&'a [u8]> {
        let mut tid = tid;
        for _ in 0..MAX_HOT_REDIRECT_HOPS {
            if let Some(tuple) =
                self.physical_visible_tuple_slice_in_overlays_at_cid(overlays, relid, tid, curcid)
            {
                return Some(tuple);
            }
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
        None
    }

    pub(crate) fn physical_visible_tuple_slice_in_overlays_at_cid<'a>(
        &'a self,
        overlays: &'a [TransactionOverlay],
        relid: u32,
        tid: Tid,
        curcid: u32,
    ) -> Option<&'a [u8]> {
        if let [overlay] = overlays {
            let include_pending = single_overlay_tid_visibility(overlay, relid, tid, Some(curcid))?;
            if include_pending && let Some(tuple) = overlay.pending_tuple_slice(relid, tid) {
                return Some(tuple);
            }
            return self
                .relations
                .get(&relid)?
                .tuple_slice(tid, include_pending);
        }
        if overlays_invalidate_tid_before(overlays, relid, tid, curcid) {
            return None;
        }
        if overlays_clear_tid(overlays, relid, tid) {
            return None;
        }
        let owns_pending = overlays_own_inserted_tid(overlays, relid, tid);
        let include_pending = overlays_own_inserted_tid_before(overlays, relid, tid, curcid);
        if owns_pending && !include_pending {
            return None;
        }
        if include_pending && let Some(tuple) = overlay_pending_tuple_slice(overlays, relid, tid) {
            return Some(tuple);
        }
        self.relations
            .get(&relid)?
            .tuple_slice(tid, include_pending)
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
        tids.retain(|tid| {
            self.physical_visible_tuple_slice_in_overlays(&session.transaction_stack, relid, *tid)
                .is_some()
        });
        tids
    }

    pub(crate) fn next_visible_tuple_slice_in_overlays<'a>(
        &'a mut self,
        overlays: &'a [TransactionOverlay],
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
        curcid: Option<u32>,
    ) -> Option<(Tid, &'a [u8])> {
        let tid = self.next_visible_tid_in_overlays(
            overlays,
            relid,
            cursor,
            high_water_offsets,
            forward,
            curcid,
        )?;
        let tuple = match curcid {
            Some(curcid) => {
                self.physical_visible_tuple_slice_in_overlays_at_cid(overlays, relid, tid, curcid)?
            }
            None => self.physical_visible_tuple_slice_in_overlays(overlays, relid, tid)?,
        };
        Some((tid, tuple))
    }

    fn next_visible_tid_in_overlays(
        &mut self,
        overlays: &[TransactionOverlay],
        relid: u32,
        mut cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
        curcid: Option<u32>,
    ) -> Option<Tid> {
        loop {
            let tid = self.next_candidate_tid_in_overlays(
                overlays,
                relid,
                cursor,
                high_water_offsets,
                forward,
            )?;
            let visible = match curcid {
                Some(curcid) => self
                    .physical_visible_tuple_slice_in_overlays_at_cid(overlays, relid, tid, curcid),
                None => self.physical_visible_tuple_slice_in_overlays(overlays, relid, tid),
            }
            .is_some();
            if visible {
                return Some(tid);
            }
            if let Some(redirect_tid) = self.visible_update_redirect_tid_beyond_high_water(
                overlays,
                relid,
                tid,
                high_water_offsets,
                curcid,
            ) {
                return Some(redirect_tid);
            }

            cursor = if forward {
                ScanCursor::after(tid)
            } else {
                ScanCursor::before(tid)
            };
        }
    }

    fn next_candidate_tid_in_overlays(
        &self,
        overlays: &[TransactionOverlay],
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
        forward: bool,
    ) -> Option<Tid> {
        if forward {
            return self.next_forward_candidate_tid_in_overlays(
                overlays,
                relid,
                cursor,
                high_water_offsets,
            );
        }

        self.next_backward_candidate_tid_in_overlays(overlays, relid, cursor, high_water_offsets)
    }

    fn next_forward_candidate_tid_in_overlays(
        &self,
        overlays: &[TransactionOverlay],
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
    ) -> Option<Tid> {
        if cursor.offset == 0 || usize::try_from(cursor.block).ok()? >= high_water_offsets.len() {
            return None;
        }
        let start_tid = Tid {
            block: cursor.block,
            offset: cursor.offset,
        };
        let mut candidate = self.relations.get(&relid).and_then(|relation| {
            relation
                .live_tids
                .range(start_tid..)
                .find(|tid| !tid_beyond_high_water(**tid, high_water_offsets))
                .copied()
        });

        for overlay in overlays {
            let Some(tids) = overlay.inserted_tids.get(&relid) else {
                continue;
            };
            for tid in tids.iter().copied() {
                if tid < start_tid || tid_beyond_high_water(tid, high_water_offsets) {
                    continue;
                }
                candidate = Some(candidate.map_or(tid, |current| current.min(tid)));
            }
        }

        candidate
    }

    fn next_backward_candidate_tid_in_overlays(
        &self,
        overlays: &[TransactionOverlay],
        relid: u32,
        cursor: ScanCursor,
        high_water_offsets: &[u16],
    ) -> Option<Tid> {
        let end_tid = scan_backward_end_tid(cursor, high_water_offsets)?;
        let mut candidate = self.relations.get(&relid).and_then(|relation| {
            relation
                .live_tids
                .range(..=end_tid)
                .rev()
                .find(|tid| !tid_beyond_high_water(**tid, high_water_offsets))
                .copied()
        });

        for overlay in overlays {
            let Some(tids) = overlay.inserted_tids.get(&relid) else {
                continue;
            };
            for tid in tids.iter().copied() {
                if tid > end_tid || tid_beyond_high_water(tid, high_water_offsets) {
                    continue;
                }
                candidate = Some(candidate.map_or(tid, |current| current.max(tid)));
            }
        }

        candidate
    }

    fn visible_update_redirect_tid_beyond_high_water(
        &mut self,
        overlays: &[TransactionOverlay],
        relid: u32,
        tid: Tid,
        high_water_offsets: &[u16],
        curcid: Option<u32>,
    ) -> Option<Tid> {
        let next_tid = overlay_update_redirect(overlays, relid, tid).or_else(|| {
            self.relations
                .get(&relid)
                .and_then(|relation| relation.update_redirects.get(&tid))
                .copied()
        })?;
        if !tid_beyond_high_water(next_tid, high_water_offsets) {
            return None;
        }

        let redirect_tid = self.resolve_update_redirect_in_overlays_compress(overlays, relid, tid);
        if !tid_beyond_high_water(redirect_tid, high_water_offsets) {
            return None;
        }
        let visible = match curcid {
            Some(curcid) => self.physical_visible_tuple_slice_in_overlays_at_cid(
                overlays,
                relid,
                redirect_tid,
                curcid,
            )?,
            None => self.physical_visible_tuple_slice_in_overlays(overlays, relid, redirect_tid)?,
        };
        let _ = visible;
        Some(redirect_tid)
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
            if cursor.offset == 0 || usize::try_from(cursor.block).ok()? >= high_water_offsets.len()
            {
                return None;
            }
            let tid = relation
                .live_tids
                .range(
                    Tid {
                        block: cursor.block,
                        offset: cursor.offset,
                    }..,
                )
                .next()
                .copied()?;
            if tid_beyond_high_water(tid, high_water_offsets) {
                return None;
            }
            return relation.tuple_slice(tid, false).map(|tuple| (tid, tuple));
        }

        let end_tid = if cursor.block == u32::MAX {
            let block: u32 = high_water_offsets.len().checked_sub(1)?.try_into().ok()?;
            Tid {
                block,
                offset: *high_water_offsets.get(block as usize)?,
            }
        } else {
            if cursor.offset == 0 {
                return None;
            }
            Tid {
                block: cursor.block,
                offset: cursor.offset,
            }
        };
        let tid = relation.live_tids.range(..=end_tid).next_back().copied()?;
        if tid_beyond_high_water(tid, high_water_offsets) {
            return None;
        }
        relation.tuple_slice(tid, false).map(|tuple| (tid, tuple))
    }

    pub(crate) fn primary_key_lookup(
        &mut self,
        session: &SessionStorage,
        relid: u32,
        key: &IndexKey,
    ) -> Option<Tid> {
        if !session.transaction_has_visibility_deltas(relid) {
            let tid = self
                .relations
                .get(&relid)?
                .primary_key_index
                .get(key)
                .copied()?;
            let tid = self.resolve_tid_redirect_in_overlays_compress(&[], relid, tid);
            return self
                .relations
                .get(&relid)?
                .tuple_slice(tid, false)
                .map(|_| tid);
        }

        for overlay in session.transaction_stack.iter().rev() {
            if let Some(tid) = overlay
                .primary_key_inserts
                .get(&relid)
                .and_then(|entries| entries.get(key))
                .copied()
            {
                let tid = self.resolve_tid_redirect_in_overlays_compress(
                    &session.transaction_stack,
                    relid,
                    tid,
                );
                if self
                    .physical_visible_tuple_slice_in_overlays(
                        &session.transaction_stack,
                        relid,
                        tid,
                    )
                    .is_some()
                {
                    return Some(tid);
                }
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
        let tid =
            self.resolve_tid_redirect_in_overlays_compress(&session.transaction_stack, relid, tid);
        self.physical_visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
            .map(|_| tid)
    }

    pub(crate) fn primary_key_lookup_read(
        &self,
        session: &SessionStorage,
        relid: u32,
        key: &IndexKey,
    ) -> Option<Tid> {
        if !session.transaction_has_visibility_deltas(relid) {
            let tid = self
                .relations
                .get(&relid)?
                .primary_key_index
                .get(key)
                .copied()?;
            let tid = self.resolve_tid_redirect_in_overlays(&[], relid, tid);
            return self
                .relations
                .get(&relid)?
                .tuple_slice(tid, false)
                .map(|_| tid);
        }

        for overlay in session.transaction_stack.iter().rev() {
            if let Some(tid) = overlay
                .primary_key_inserts
                .get(&relid)
                .and_then(|entries| entries.get(key))
                .copied()
            {
                let tid =
                    self.resolve_tid_redirect_in_overlays(&session.transaction_stack, relid, tid);
                if self
                    .physical_visible_tuple_slice_in_overlays(
                        &session.transaction_stack,
                        relid,
                        tid,
                    )
                    .is_some()
                {
                    return Some(tid);
                }
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
        self.physical_visible_tuple_slice_in_overlays(&session.transaction_stack, relid, tid)
            .map(|_| tid)
    }

    pub(crate) fn find_visible_by_index_key_excluding(
        &mut self,
        session: &SessionStorage,
        relid: u32,
        index_spec: &UniqueIndexSpec,
        key: &IndexKey,
        replacing_tid: Option<Tid>,
    ) -> Option<Tid> {
        let replacing_tid = replacing_tid.map(|tid| {
            self.resolve_tid_redirect_in_overlays_compress(&session.transaction_stack, relid, tid)
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

    pub(crate) fn find_visible_by_index_key_excluding_read(
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
            if let Some(tid) = self.primary_key_lookup_read(session, relid, key)
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
        &mut self,
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

    pub(crate) fn unique_index_conflict_for_input_read(
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
                .find_visible_by_index_key_excluding_read(
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

#[derive(Clone, Copy)]
pub(crate) struct InsertMetadata {
    pub(crate) xid: u32,
    pub(crate) cid: u32,
}

static STORAGE: OnceLock<RwLock<StorageState>> = OnceLock::new();

pub(crate) fn storage() -> &'static RwLock<StorageState> {
    STORAGE.get_or_init(|| RwLock::new(StorageState::default()))
}

fn row_counts() -> &'static Mutex<HashMap<u32, Arc<AtomicUsize>>> {
    STORAGE2_ROW_COUNTS.get_or_init(|| Mutex::new(HashMap::default()))
}

thread_local! {
    static ROW_COUNT_COUNTER_CACHE: RefCell<Vec<(u32, Arc<AtomicUsize>)>> = const { RefCell::new(Vec::new()) };
}

fn cached_row_count_counter(relid: u32) -> Option<Arc<AtomicUsize>> {
    ROW_COUNT_COUNTER_CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(cached_relid, _)| *cached_relid == relid)
            .map(|(_, counter)| counter.clone())
    })
}

fn remember_row_count_counter(relid: u32, counter: Arc<AtomicUsize>) {
    ROW_COUNT_COUNTER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, existing)) = cache
            .iter_mut()
            .find(|(cached_relid, _)| *cached_relid == relid)
        {
            *existing = counter;
            return;
        }
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push((relid, counter));
    });
}

fn row_count_counter(relid: u32) -> Arc<AtomicUsize> {
    let mut counts = row_counts().lock();
    counts
        .entry(relid)
        .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
        .clone()
}

fn store_committed_row_count(relid: u32, row_count: usize) {
    if let Some(counter) = cached_row_count_counter(relid) {
        counter.store(row_count, Ordering::Relaxed);
        return;
    }

    let counter = row_count_counter(relid);
    counter.store(row_count, Ordering::Relaxed);
    remember_row_count_counter(relid, counter);
}

fn load_committed_row_count(relid: u32) -> usize {
    let counter = cached_row_count_counter(relid).or_else(|| {
        let counter = {
            let counts = row_counts().lock();
            counts.get(&relid).cloned()
        };
        if let Some(counter) = counter.as_ref() {
            remember_row_count_counter(relid, counter.clone());
        }
        counter
    });
    counter
        .map(|counter| counter.load(Ordering::Relaxed))
        .unwrap_or_default()
}

pub(crate) fn committed_row_count_cached(relid: u32) -> usize {
    load_committed_row_count(relid)
}

pub(crate) fn visible_row_count_cached(relid: u32) -> usize {
    let committed = load_committed_row_count(relid);
    with_current_session_storage(|session| {
        if !session.transaction_has_visibility_deltas(relid) {
            return committed;
        }
        if let Some(delta) = session.single_overlay_row_count_delta(relid) {
            return if delta >= 0 {
                committed.saturating_add(delta as usize)
            } else {
                committed.saturating_sub(delta.unsigned_abs())
            };
        }
        committed
            .saturating_add(session.transaction_visible_insert_count(relid))
            .saturating_sub(session.transaction_invalidated_live_count(relid))
    })
}

pub(crate) fn with_storage<R>(f: impl FnOnce(&mut StorageState, &mut SessionStorage) -> R) -> R {
    with_current_session_storage(|session| f(&mut storage().write(), session))
}

pub(crate) fn with_storage_read<R>(f: impl FnOnce(&StorageState, &mut SessionStorage) -> R) -> R {
    with_current_session_storage(|session| f(&storage().read(), session))
}

pub(crate) fn with_session_storage<R>(f: impl FnOnce(&mut SessionStorage) -> R) -> R {
    with_current_session_storage(f)
}

fn append_pending_input_tuple_current_session(
    relid: u32,
    input: &RowInput<'_>,
) -> Result<Tid, CatalogError> {
    let tuple_len = tuple_storage_len(input)?;
    if let Some(tid) = with_session_storage(|session| -> Result<Option<Tid>, CatalogError> {
        session.ensure_transaction();
        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        let Some(tid) =
            overlay.append_pending_input_tuple_to_existing_page(relid, input, tuple_len)?
        else {
            return Ok(None);
        };
        overlay.insert_tid(relid, tid);
        Ok(Some(tid))
    })? {
        return Ok(tid);
    }

    with_storage(|state, session| {
        state.append_pending_input_tuple_with_len(session, relid, input, tuple_len)
    })
}

fn record_current_session_insert_metadata(relid: u32, tid: Tid, metadata: InsertMetadata) -> bool {
    with_session_storage(|session| {
        let Some(overlay) = session.transaction_stack.last_mut() else {
            return false;
        };
        overlay.set_insert_cid(relid, tid, metadata.cid);
        overlay.set_pending_insert_metadata(relid, tid, metadata.xid, metadata.cid)
    })
}

fn record_current_session_primary_key_insert(relid: u32, input: &RowInput<'_>, tid: Tid) {
    if let Some(index_spec) = primary_index_spec_for_relation_oid(Oid(relid))
        && let Some(key) = index_key_for_input(&index_spec, input)
    {
        with_session_storage(|session| {
            session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured")
                .insert_primary_key(relid, key, tid);
        });
    }
}

fn resolve_update_redirect_current_session(relid: u32, mut tid: Tid) -> Tid {
    with_session_storage(|session| {
        for _ in 0..1_000_000 {
            let Some(next_tid) = overlay_update_redirect(&session.transaction_stack, relid, tid)
            else {
                break;
            };
            tid = next_tid;
        }
        tid
    })
}

fn update_current_session_pending_hot(
    relid: u32,
    old_tid: Tid,
    input: &RowInput<'_>,
    record_hot_redirect: bool,
    metadata: Option<UpdateMetadata>,
) -> Result<Option<Tid>, CatalogError> {
    let old_tid = resolve_update_redirect_current_session(relid, old_tid);
    let tuple_len = tuple_storage_len(input)?;
    with_session_storage(|session| -> Result<Option<Tid>, CatalogError> {
        if !session.owns_inserted_tid(relid, old_tid)
            || overlays_invalidate_tid(&session.transaction_stack, relid, old_tid)
            || overlay_pending_tuple_slice(&session.transaction_stack, relid, old_tid).is_none()
        {
            return Ok(None);
        }

        let Some(overlay) = session.transaction_stack.last_mut() else {
            return Ok(None);
        };
        let Some(new_tid) =
            overlay.append_pending_input_tuple_to_existing_page(relid, input, tuple_len)?
        else {
            return Ok(None);
        };
        overlay.insert_tid(relid, new_tid);
        overlay.invalidate(relid, old_tid);
        overlay.insert_update_redirect(relid, old_tid, new_tid);
        if record_hot_redirect {
            overlay.insert_hot_redirect(relid, old_tid, new_tid);
        }
        if let Some(metadata) = metadata {
            overlay.set_invalidate_metadata(
                relid,
                old_tid,
                metadata.delete_xid,
                metadata.delete_cid,
            );
            overlay.set_insert_cid(relid, new_tid, metadata.insert_cid);
            if !overlay.set_pending_insert_metadata(
                relid,
                new_tid,
                metadata.insert_xid,
                metadata.insert_cid,
            ) || !overlay.set_pending_row_xmax(relid, new_tid, metadata.row_xmax)
            {
                return Err(storage_limit_error(
                    "storage2 could not record pending update metadata",
                ));
            }
        }
        session.mark_scans_visibility_delta(relid);
        Ok(Some(new_tid))
    })
}

fn update_current_session_pending_if_single_byval_preserved(
    relid: u32,
    old_tid: Tid,
    input: &RowInput<'_>,
    key: SingleByvalHotKey,
    metadata: Option<UpdateMetadata>,
) -> Result<Option<Tid>, CatalogError> {
    let old_tid = resolve_update_redirect_current_session(relid, old_tid);
    let hot_preserved = with_session_storage(|session| {
        if !session.owns_inserted_tid(relid, old_tid)
            || overlays_invalidate_tid(&session.transaction_stack, relid, old_tid)
        {
            return None;
        }
        let old_tuple_slice =
            overlay_pending_tuple_slice(&session.transaction_stack, relid, old_tid)?;
        tuple_byval_attr_equals(old_tuple_slice, key.attnum, key.value, key.is_null)
    })
    .unwrap_or(false);

    if !hot_preserved {
        return Ok(None);
    }

    update_current_session_pending_hot(relid, old_tid, input, true, metadata)
}

pub(crate) fn relation_insert_impl(
    relid: u32,
    input: RowInput<'_>,
    tid_out: *mut u64,
    unique_check: UniqueCheck,
    metadata: Option<InsertMetadata>,
    record_primary_key: bool,
) -> bool {
    let result = (|| -> Result<Option<Tid>, CatalogError> {
        if matches!(unique_check, UniqueCheck::Enforce)
            && with_storage_read(|state, session| {
                state
                    .unique_index_conflict_for_input_read(session, relid, &input, None)
                    .is_some()
            })
        {
            return Ok(None);
        }

        let tid = append_pending_input_tuple_current_session(relid, &input)?;
        if let Some(metadata) = metadata
            && !record_current_session_insert_metadata(relid, tid, metadata)
        {
            with_storage(|state, _session| {
                state.set_insert_metadata(relid, tid, metadata.xid, metadata.cid);
            });
        }

        if record_primary_key {
            record_current_session_primary_key_insert(relid, &input, tid);
        }
        Ok(Some(tid))
    })();

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

fn tid_beyond_high_water(tid: Tid, high_water_offsets: &[u16]) -> bool {
    high_water_offsets
        .get(tid.block as usize)
        .is_none_or(|max_offset| tid.offset > *max_offset)
}

fn scan_backward_end_tid(cursor: ScanCursor, high_water_offsets: &[u16]) -> Option<Tid> {
    if cursor.block == u32::MAX {
        let (block, offset) = high_water_offsets
            .iter()
            .enumerate()
            .rev()
            .find(|(_, offset)| **offset > 0)?;
        return Some(Tid {
            block: block.try_into().ok()?,
            offset: *offset,
        });
    }

    if cursor.offset == 0 || usize::try_from(cursor.block).ok()? >= high_water_offsets.len() {
        return None;
    }

    Some(Tid {
        block: cursor.block,
        offset: cursor.offset,
    })
}

pub(crate) fn relation_update_impl(
    relid: u32,
    packed_tid: u64,
    input: RowInput<'_>,
    new_tid_out: *mut u64,
    unique_check: UniqueCheck,
    record_hot_redirect: bool,
    metadata: Option<UpdateMetadata>,
) -> bool {
    let Some(old_tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    let hot_unchecked = record_hot_redirect && matches!(unique_check, UniqueCheck::Skip);
    if hot_unchecked {
        match update_current_session_pending_hot(
            relid,
            old_tid,
            &input,
            record_hot_redirect,
            metadata,
        ) {
            Ok(Some(tid)) => {
                if !new_tid_out.is_null() {
                    unsafe {
                        *new_tid_out = tid.pack();
                    }
                }
                return true;
            }
            Ok(None) => {}
            Err(error) => {
                set_last_storage_error(error);
                return false;
            }
        }
    }

    let result = with_storage(|state, session| -> Result<Option<Tid>, CatalogError> {
        let has_visibility_deltas = session.transaction_has_visibility_deltas(relid);
        let old_tid = state.resolve_update_redirect_in_overlays_compress(
            if has_visibility_deltas {
                &session.transaction_stack
            } else {
                &[]
            },
            relid,
            old_tid,
        );
        let old_tid = state.resolve_tid_redirect_in_overlays_compress(
            if has_visibility_deltas {
                &session.transaction_stack
            } else {
                &[]
            },
            relid,
            old_tid,
        );
        let Some(old_tuple_slice) = state.physical_visible_tuple_slice_for_session(
            session,
            relid,
            old_tid,
            has_visibility_deltas,
        ) else {
            return Ok(None);
        };
        let old_primary_key = if hot_unchecked {
            None
        } else {
            let Some(old_tuple) = decode_tuple(old_tid, old_tuple_slice) else {
                return Ok(None);
            };
            primary_index_spec_for_relation_oid(Oid(relid))
                .and_then(|index_spec| index_key_for_decoded(&index_spec, &old_tuple.values))
        };
        let old_is_own_insert = !hot_unchecked && session.owns_inserted_tid(relid, old_tid);
        if matches!(unique_check, UniqueCheck::Enforce)
            && state
                .unique_index_conflict_for_input(session, relid, &input, Some(old_tid))
                .is_some()
        {
            return Ok(None);
        }

        let new_tid = state.append_pending_input_tuple(session, relid, &input)?;
        let new_primary_key = if hot_unchecked {
            None
        } else {
            primary_index_spec_for_relation_oid(Oid(relid))
                .and_then(|index_spec| index_key_for_input(&index_spec, &input))
        };

        let overlay = session
            .transaction_stack
            .last_mut()
            .expect("transaction was just ensured");
        overlay.invalidate(relid, old_tid);
        overlay.insert_update_redirect(relid, old_tid, new_tid);
        if record_hot_redirect {
            overlay.insert_hot_redirect(relid, old_tid, new_tid);
        }
        if let Some(metadata) = metadata {
            overlay.set_invalidate_metadata(
                relid,
                old_tid,
                metadata.delete_xid,
                metadata.delete_cid,
            );
            overlay.set_insert_cid(relid, new_tid, metadata.insert_cid);
            if !overlay.set_pending_insert_metadata(
                relid,
                new_tid,
                metadata.insert_xid,
                metadata.insert_cid,
            ) {
                state.set_insert_metadata(relid, new_tid, metadata.insert_xid, metadata.insert_cid);
            }
            if !overlay.set_pending_row_xmax(relid, new_tid, metadata.row_xmax) {
                state.set_row_xmax(relid, new_tid, metadata.row_xmax);
            }
        }
        if old_primary_key.is_some()
            && old_primary_key != new_primary_key
            && let Some(key) = old_primary_key
        {
            if old_is_own_insert {
                overlay.remove_primary_key_insert(relid, &key);
            } else {
                overlay.delete_primary_key(relid, key);
            }
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

#[derive(Clone, Copy, Debug)]
pub(crate) struct UpdateMetadata {
    pub(crate) delete_xid: u32,
    pub(crate) delete_cid: u32,
    pub(crate) insert_xid: u32,
    pub(crate) insert_cid: u32,
    pub(crate) row_xmax: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SingleByvalHotKey {
    pub(crate) attnum: usize,
    pub(crate) value: usize,
    pub(crate) is_null: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HotUpdateOutputs {
    pub(crate) new_tid: *mut u64,
    pub(crate) hot_preserved: *mut bool,
}

pub(crate) fn relation_update_hot_if_single_byval_preserved_impl(
    relid: u32,
    packed_tid: u64,
    input: RowInput<'_>,
    key: SingleByvalHotKey,
    outputs: HotUpdateOutputs,
    metadata: Option<UpdateMetadata>,
) -> bool {
    let Some(old_tid) = Tid::unpack(packed_tid) else {
        return false;
    };
    if key.attnum == 0 {
        return false;
    }
    let old_tid = resolve_update_redirect_current_session(relid, old_tid);
    match update_current_session_pending_if_single_byval_preserved(
        relid, old_tid, &input, key, metadata,
    ) {
        Ok(Some(tid)) => {
            if !outputs.new_tid.is_null() {
                unsafe {
                    *outputs.new_tid = tid.pack();
                }
            }
            if !outputs.hot_preserved.is_null() {
                unsafe {
                    *outputs.hot_preserved = true;
                }
            }
            return true;
        }
        Ok(None) => {}
        Err(error) => {
            set_last_storage_error(error);
            return false;
        }
    }

    let result = with_storage(
        |state, session| -> Result<Option<(Tid, bool)>, CatalogError> {
            let has_visibility_deltas = session.transaction_has_visibility_deltas(relid);
            let old_tid = state.resolve_update_redirect_in_overlays_compress(
                if has_visibility_deltas {
                    &session.transaction_stack
                } else {
                    &[]
                },
                relid,
                old_tid,
            );
            let old_tid = state.resolve_tid_redirect_in_overlays_compress(
                if has_visibility_deltas {
                    &session.transaction_stack
                } else {
                    &[]
                },
                relid,
                old_tid,
            );
            let Some(old_tuple_slice) = state.physical_visible_tuple_slice_for_session(
                session,
                relid,
                old_tid,
                has_visibility_deltas,
            ) else {
                return Ok(None);
            };
            let hot_preserved =
                tuple_byval_attr_equals(old_tuple_slice, key.attnum, key.value, key.is_null)
                    .unwrap_or(false);
            let old_primary_key = if hot_preserved {
                None
            } else {
                let Some(old_tuple) = decode_tuple(old_tid, old_tuple_slice) else {
                    return Ok(None);
                };
                primary_index_spec_for_relation_oid(Oid(relid))
                    .and_then(|index_spec| index_key_for_decoded(&index_spec, &old_tuple.values))
            };
            let old_is_own_insert = !hot_preserved && session.owns_inserted_tid(relid, old_tid);

            let new_tid = state.append_pending_input_tuple(session, relid, &input)?;
            let new_primary_key = if hot_preserved {
                None
            } else {
                primary_index_spec_for_relation_oid(Oid(relid))
                    .and_then(|index_spec| index_key_for_input(&index_spec, &input))
            };

            let overlay = session
                .transaction_stack
                .last_mut()
                .expect("transaction was just ensured");
            overlay.invalidate(relid, old_tid);
            overlay.insert_update_redirect(relid, old_tid, new_tid);
            if hot_preserved {
                overlay.insert_hot_redirect(relid, old_tid, new_tid);
            }
            if let Some(metadata) = metadata {
                overlay.set_invalidate_metadata(
                    relid,
                    old_tid,
                    metadata.delete_xid,
                    metadata.delete_cid,
                );
                overlay.set_insert_cid(relid, new_tid, metadata.insert_cid);
                if !overlay.set_pending_insert_metadata(
                    relid,
                    new_tid,
                    metadata.insert_xid,
                    metadata.insert_cid,
                ) {
                    state.set_insert_metadata(
                        relid,
                        new_tid,
                        metadata.insert_xid,
                        metadata.insert_cid,
                    );
                }
                if !overlay.set_pending_row_xmax(relid, new_tid, metadata.row_xmax) {
                    state.set_row_xmax(relid, new_tid, metadata.row_xmax);
                }
            }
            if old_primary_key.is_some()
                && old_primary_key != new_primary_key
                && let Some(key) = old_primary_key
            {
                if old_is_own_insert {
                    overlay.remove_primary_key_insert(relid, &key);
                } else {
                    overlay.delete_primary_key(relid, key);
                }
            }
            if let Some(key) = new_primary_key {
                overlay.insert_primary_key(relid, key, new_tid);
            }
            session.mark_scans_visibility_delta(relid);
            Ok(Some((new_tid, hot_preserved)))
        },
    );

    match result {
        Ok(Some((tid, hot_preserved))) => {
            if !outputs.new_tid.is_null() {
                unsafe {
                    *outputs.new_tid = tid.pack();
                }
            }
            if !outputs.hot_preserved.is_null() {
                unsafe {
                    *outputs.hot_preserved = hot_preserved;
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

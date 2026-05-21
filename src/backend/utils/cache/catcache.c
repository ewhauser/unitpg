/*-------------------------------------------------------------------------
 *
 * catcache.c
 *	  System catalog cache for tuples matching a key.
 *
 * Portions Copyright (c) 1996-2026, PostgreSQL Global Development Group
 * Portions Copyright (c) 1994, Regents of the University of California
 *
 *
 * IDENTIFICATION
 *	  src/backend/utils/cache/catcache.c
 *
 *-------------------------------------------------------------------------
 */
#include "postgres.h"

#ifdef USE_FASTPG
#include "access/fastpg_catalog.h"
#include "access/multixact.h"
#include "access/transam.h"
#endif
#include "access/genam.h"
#include "access/heaptoast.h"
#include "access/htup_details.h"
#include "access/relscan.h"
#include "access/sysattr.h"
#include "access/table.h"
#include "access/xact.h"
#include "catalog/catalog.h"
#include "catalog/heap.h"
#include "catalog/pg_aggregate.h"
#include "catalog/pg_am.h"
#include "catalog/pg_amop.h"
#include "catalog/pg_amproc.h"
#include "catalog/pg_attribute.h"
#include "catalog/pg_auth_members.h"
#include "catalog/pg_authid.h"
#include "catalog/pg_cast.h"
#include "catalog/pg_class.h"
#include "catalog/pg_collation.h"
#include "catalog/pg_constraint.h"
#include "catalog/pg_conversion.h"
#include "catalog/pg_database.h"
#include "catalog/pg_default_acl.h"
#include "catalog/pg_enum.h"
#include "catalog/pg_event_trigger.h"
#include "catalog/pg_extension.h"
#include "catalog/pg_foreign_data_wrapper.h"
#include "catalog/pg_foreign_server.h"
#include "catalog/pg_foreign_table.h"
#include "catalog/pg_index.h"
#include "catalog/pg_language.h"
#include "catalog/pg_namespace.h"
#include "catalog/pg_opclass.h"
#include "catalog/pg_operator.h"
#include "catalog/pg_opfamily.h"
#include "catalog/pg_parameter_acl.h"
#include "catalog/pg_partitioned_table.h"
#include "catalog/pg_proc.h"
#include "catalog/pg_propgraph_element.h"
#include "catalog/pg_propgraph_element_label.h"
#include "catalog/pg_propgraph_label.h"
#include "catalog/pg_propgraph_label_property.h"
#include "catalog/pg_propgraph_property.h"
#include "catalog/pg_publication.h"
#include "catalog/pg_publication_namespace.h"
#include "catalog/pg_publication_rel.h"
#include "catalog/pg_range.h"
#include "catalog/pg_replication_origin.h"
#include "catalog/pg_rewrite.h"
#include "catalog/pg_sequence.h"
#include "catalog/pg_statistic.h"
#include "catalog/pg_statistic_ext.h"
#include "catalog/pg_statistic_ext_data.h"
#include "catalog/pg_subscription.h"
#include "catalog/pg_subscription_rel.h"
#include "catalog/pg_tablespace.h"
#include "catalog/pg_transform.h"
#include "catalog/pg_ts_config.h"
#include "catalog/pg_ts_config_map.h"
#include "catalog/pg_ts_dict.h"
#include "catalog/pg_ts_parser.h"
#include "catalog/pg_ts_template.h"
#include "catalog/pg_type.h"
#include "catalog/pg_user_mapping.h"
#include "common/hashfn.h"
#include "common/pg_prng.h"
#include "miscadmin.h"
#include "port/pg_bitutils.h"
#ifdef CATCACHE_STATS
#include "storage/ipc.h"		/* for on_proc_exit */
#endif
#include "storage/lmgr.h"
#include "utils/builtins.h"
#include "utils/catcache.h"
#include "utils/datum.h"
#include "utils/fmgroids.h"
#include "utils/injection_point.h"
#include "utils/inval.h"
#include "utils/memutils.h"
#include "utils/rel.h"
#include "utils/resowner.h"
#include "utils/syscache.h"

/*
 * If a catcache invalidation is processed while we are in the middle of
 * creating a catcache entry (or list), it might apply to the entry we're
 * creating, making it invalid before it's been inserted to the catcache.  To
 * catch such cases, we have a stack of "create-in-progress" entries.  Cache
 * invalidation marks any matching entries in the stack as dead, in addition
 * to the actual CatCTup and CatCList entries.
 */
typedef struct CatCInProgress
{
	CatCache   *cache;			/* cache that the entry belongs to */
	uint32		hash_value;		/* hash of the entry; ignored for lists */
	bool		list;			/* is it a list entry? */
	bool		dead;			/* set when the entry is invalidated */
	struct CatCInProgress *next;
} CatCInProgress;

static CatCInProgress *catcache_in_progress_stack = NULL;

 /* #define CACHEDEBUG */	/* turns DEBUG elogs on */

/*
 * Given a hash value and the size of the hash table, find the bucket
 * in which the hash value belongs. Since the hash table must contain
 * a power-of-2 number of elements, this is a simple bitmask.
 */
#define HASH_INDEX(h, sz) ((Index) ((h) & ((sz) - 1)))


/*
 *		variables, macros and other stuff
 */

#ifdef CACHEDEBUG
#define CACHE_elog(...)				elog(__VA_ARGS__)
#else
#define CACHE_elog(...)
#endif

/* Cache management header --- pointer is NULL until created */
static CatCacheHeader *CacheHdr = NULL;

static inline HeapTuple SearchCatCacheInternal(CatCache *cache,
											   int nkeys,
											   Datum v1, Datum v2,
											   Datum v3, Datum v4);

static pg_noinline HeapTuple SearchCatCacheMiss(CatCache *cache,
												int nkeys,
												uint32 hashValue,
												Index hashIndex,
												Datum v1, Datum v2,
												Datum v3, Datum v4);

static uint32 CatalogCacheComputeHashValue(CatCache *cache, int nkeys,
										   Datum v1, Datum v2, Datum v3, Datum v4);
static uint32 CatalogCacheComputeTupleHashValue(CatCache *cache, int nkeys,
												HeapTuple tuple);
static inline bool CatalogCacheCompareTuple(const CatCache *cache, int nkeys,
											const Datum *cachekeys,
											const Datum *searchkeys);
#ifdef USE_FASTPG
static HeapTuple FastPgCatalogCacheLookupGeneric(CatCache *cache, int nkeys,
												 Datum *arguments);
#endif

#ifdef CATCACHE_STATS
static void CatCachePrintStats(int code, Datum arg);
#endif
static void CatCacheRemoveCTup(CatCache *cache, CatCTup *ct);
static void CatCacheRemoveCList(CatCache *cache, CatCList *cl);
static void RehashCatCache(CatCache *cp);
static void RehashCatCacheLists(CatCache *cp);
static void CatalogCacheInitializeCache(CatCache *cache);
static CatCTup *CatalogCacheCreateEntry(CatCache *cache, HeapTuple ntp,
										Datum *arguments,
										uint32 hashValue, Index hashIndex);

static void ReleaseCatCacheWithOwner(HeapTuple tuple, ResourceOwner resowner);
static void ReleaseCatCacheListWithOwner(CatCList *list, ResourceOwner resowner);
static void CatCacheFreeKeys(TupleDesc tupdesc, int nkeys, const int *attnos,
							 const Datum *keys);
static void CatCacheCopyKeys(TupleDesc tupdesc, int nkeys, const int *attnos,
							 const Datum *srckeys, Datum *dstkeys);
#ifdef USE_FASTPG
static bool FastPgCatalogCacheInitializeCache(CatCache *cache);
static bool FastPgCatalogCacheHandlesMiss(CatCache *cache, int nkeys);
static bool FastPgCatalogCacheIsEmpty(CatCache *cache);
static bool FastPgCatalogRowIdToTid(uint64_t row_id, ItemPointer tid);
static HeapTuple FastPgCatalogCacheLookup(CatCache *cache, int nkeys,
										  Datum *arguments);
static CatCList *FastPgCatalogCacheBuildList(CatCache *cache, int nkeys,
											 Datum *arguments,
											 uint32 lHashValue,
											 dlist_head *lbucket);
static CatCList *FastPgCatalogCacheBuildEmptyList(CatCache *cache, int nkeys,
												  Datum *arguments,
												  uint32 lHashValue,
												  dlist_head *lbucket);
static HeapTuple FastPgBuildClassTuple(TupleDesc tupdesc,
									   const FastPgRustCatalogRelation *relation);
static HeapTuple FastPgBuildAttributeTuple(TupleDesc tupdesc,
										   Oid relation_oid, AttrNumber attnum,
										   const FastPgRustCatalogColumn *column);
static HeapTuple FastPgBuildProcTuple(TupleDesc tupdesc,
									  const FastPgRustCatalogProc *proc);
static HeapTuple FastPgBuildAggregateTuple(TupleDesc tupdesc,
										   const FastPgRustCatalogAggregate *agg);
static HeapTuple FastPgBuildOperatorTuple(TupleDesc tupdesc,
										  const FastPgRustCatalogOperator *oper);
static HeapTuple FastPgBuildTypeTuple(TupleDesc tupdesc,
									  const FastPgRustCatalogType *type);
#endif


/*
 *					internal support functions
 */

/* ResourceOwner callbacks to hold catcache references */

static void ResOwnerReleaseCatCache(Datum res);
static char *ResOwnerPrintCatCache(Datum res);
static void ResOwnerReleaseCatCacheList(Datum res);
static char *ResOwnerPrintCatCacheList(Datum res);

static const ResourceOwnerDesc catcache_resowner_desc =
{
	/* catcache references */
	.name = "catcache reference",
	.release_phase = RESOURCE_RELEASE_AFTER_LOCKS,
	.release_priority = RELEASE_PRIO_CATCACHE_REFS,
	.ReleaseResource = ResOwnerReleaseCatCache,
	.DebugPrint = ResOwnerPrintCatCache
};

static const ResourceOwnerDesc catlistref_resowner_desc =
{
	/* catcache-list pins */
	.name = "catcache list reference",
	.release_phase = RESOURCE_RELEASE_AFTER_LOCKS,
	.release_priority = RELEASE_PRIO_CATCACHE_LIST_REFS,
	.ReleaseResource = ResOwnerReleaseCatCacheList,
	.DebugPrint = ResOwnerPrintCatCacheList
};

/* Convenience wrappers over ResourceOwnerRemember/Forget */
static inline void
ResourceOwnerRememberCatCacheRef(ResourceOwner owner, HeapTuple tuple)
{
	ResourceOwnerRemember(owner, PointerGetDatum(tuple), &catcache_resowner_desc);
}
static inline void
ResourceOwnerForgetCatCacheRef(ResourceOwner owner, HeapTuple tuple)
{
	ResourceOwnerForget(owner, PointerGetDatum(tuple), &catcache_resowner_desc);
}
static inline void
ResourceOwnerRememberCatCacheListRef(ResourceOwner owner, CatCList *list)
{
	ResourceOwnerRemember(owner, PointerGetDatum(list), &catlistref_resowner_desc);
}
static inline void
ResourceOwnerForgetCatCacheListRef(ResourceOwner owner, CatCList *list)
{
	ResourceOwnerForget(owner, PointerGetDatum(list), &catlistref_resowner_desc);
}


/*
 * Hash and equality functions for system types that are used as cache key
 * fields.  In some cases, we just call the regular SQL-callable functions for
 * the appropriate data type, but that tends to be a little slow, and the
 * speed of these functions is performance-critical.  Therefore, for data
 * types that frequently occur as catcache keys, we hard-code the logic here.
 * Avoiding the overhead of DirectFunctionCallN(...) is a substantial win, and
 * in certain cases (like int4) we can adopt a faster hash algorithm as well.
 */

static bool
chareqfast(Datum a, Datum b)
{
	return DatumGetChar(a) == DatumGetChar(b);
}

static uint32
charhashfast(Datum datum)
{
	return murmurhash32((int32) DatumGetChar(datum));
}

static bool
nameeqfast(Datum a, Datum b)
{
	char	   *ca = NameStr(*DatumGetName(a));
	char	   *cb = NameStr(*DatumGetName(b));

	/*
	 * Catalogs only use deterministic collations, so ignore column collation
	 * and use fast path.
	 */
	return strncmp(ca, cb, NAMEDATALEN) == 0;
}

static uint32
namehashfast(Datum datum)
{
	char	   *key = NameStr(*DatumGetName(datum));

	/*
	 * Catalogs only use deterministic collations, so ignore column collation
	 * and use fast path.
	 */
	return hash_bytes((unsigned char *) key, strlen(key));
}

static bool
int2eqfast(Datum a, Datum b)
{
	return DatumGetInt16(a) == DatumGetInt16(b);
}

static uint32
int2hashfast(Datum datum)
{
	return murmurhash32((int32) DatumGetInt16(datum));
}

static bool
int4eqfast(Datum a, Datum b)
{
	return DatumGetInt32(a) == DatumGetInt32(b);
}

static uint32
int4hashfast(Datum datum)
{
	return murmurhash32((int32) DatumGetInt32(datum));
}

static bool
texteqfast(Datum a, Datum b)
{
	/*
	 * Catalogs only use deterministic collations, so ignore column collation
	 * and use "C" locale for efficiency.
	 */
	return DatumGetBool(DirectFunctionCall2Coll(texteq, C_COLLATION_OID, a, b));
}

static uint32
texthashfast(Datum datum)
{
	/*
	 * Catalogs only use deterministic collations, so ignore column collation
	 * and use "C" locale for efficiency.
	 */
	return DatumGetInt32(DirectFunctionCall1Coll(hashtext, C_COLLATION_OID, datum));
}

static bool
oidvectoreqfast(Datum a, Datum b)
{
	return DatumGetBool(DirectFunctionCall2(oidvectoreq, a, b));
}

static uint32
oidvectorhashfast(Datum datum)
{
	return DatumGetInt32(DirectFunctionCall1(hashoidvector, datum));
}

/* Lookup support functions for a type. */
static void
GetCCHashEqFuncs(Oid keytype, CCHashFN *hashfunc, RegProcedure *eqfunc, CCFastEqualFN *fasteqfunc)
{
	switch (keytype)
	{
		case BOOLOID:
			*hashfunc = charhashfast;
			*fasteqfunc = chareqfast;
			*eqfunc = F_BOOLEQ;
			break;
		case CHAROID:
			*hashfunc = charhashfast;
			*fasteqfunc = chareqfast;
			*eqfunc = F_CHAREQ;
			break;
		case NAMEOID:
			*hashfunc = namehashfast;
			*fasteqfunc = nameeqfast;
			*eqfunc = F_NAMEEQ;
			break;
		case INT2OID:
			*hashfunc = int2hashfast;
			*fasteqfunc = int2eqfast;
			*eqfunc = F_INT2EQ;
			break;
		case INT4OID:
			*hashfunc = int4hashfast;
			*fasteqfunc = int4eqfast;
			*eqfunc = F_INT4EQ;
			break;
		case TEXTOID:
			*hashfunc = texthashfast;
			*fasteqfunc = texteqfast;
			*eqfunc = F_TEXTEQ;
			break;
		case OIDOID:
		case REGPROCOID:
		case REGPROCEDUREOID:
		case REGOPEROID:
		case REGOPERATOROID:
		case REGCLASSOID:
		case REGTYPEOID:
		case REGCOLLATIONOID:
		case REGCONFIGOID:
		case REGDICTIONARYOID:
		case REGROLEOID:
		case REGNAMESPACEOID:
		case REGDATABASEOID:
			*hashfunc = int4hashfast;
			*fasteqfunc = int4eqfast;
			*eqfunc = F_OIDEQ;
			break;
		case OIDVECTOROID:
			*hashfunc = oidvectorhashfast;
			*fasteqfunc = oidvectoreqfast;
			*eqfunc = F_OIDVECTOREQ;
			break;
		default:
			elog(FATAL, "type %u not supported as catcache key", keytype);
			*hashfunc = NULL;	/* keep compiler quiet */

			*eqfunc = InvalidOid;
			break;
	}
}

#ifdef USE_FASTPG
extern uint64_t fastpg_rust_scan_begin(uint32_t relid);
extern uint64_t fastpg_rust_scan_begin_filtered(uint32_t relid,
												const int16_t *attnums,
												const uintptr_t *values,
												size_t nkeys);
extern void fastpg_rust_scan_end(uint64_t scan_handle);
extern bool fastpg_rust_scan_next(uint64_t scan_handle,
								  uint8_t forward,
								  uintptr_t *values,
								  uint8_t *isnull,
								  size_t natts,
								  uint64_t *row_id);

static void
FastPgTupleDescInitEntryWithTypmod(TupleDesc tupdesc, AttrNumber attrNumber,
								   const char *attributeName, Oid oidtypeid,
								   int32 typmod, bool notnull)
{
	TupleDescInitBuiltinEntry(tupdesc, attrNumber, attributeName, oidtypeid, typmod, 0);
	TupleDescAttr(tupdesc, attrNumber - 1)->attnotnull = notnull;
}

static TupleDesc
FastPgBuildRustCatalogTupleDesc(Oid relation_oid)
{
	FastPgRustCatalogRelation relation;
	TupleDesc	tupdesc;

	if (!fastpg_rust_catalog_relation_by_oid((uint32_t) relation_oid, &relation))
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg generated catalog has no tuple descriptor for relation %u",
						relation_oid)));

	tupdesc = CreateTemplateTupleDesc(relation.column_count);
	for (uint16_t index = 0; index < relation.column_count; index++)
	{
		FastPgRustCatalogColumn column;

		if (!fastpg_rust_catalog_relation_column_by_index((uint32_t) relation_oid,
														  (size_t) index,
														  &column))
			ereport(ERROR,
					(errcode(ERRCODE_UNDEFINED_COLUMN),
					 errmsg("fastpg virtual catalog %u is missing generated column %u",
							relation_oid, index + 1)));

		FastPgTupleDescInitEntryWithTypmod(tupdesc,
										   (AttrNumber) (index + 1),
										   column.name,
										   (Oid) column.type_oid,
										   column.type_mod,
										   column.is_not_null != 0);
	}

	return tupdesc;
}

static bool
FastPgCatalogCacheInitializeCache(CatCache *cache)
{
	MemoryContext oldcxt;
	TupleDesc	tupdesc = NULL;
	const char *relname = NULL;
	FastPgRustCatalogRelation relation;
	int			i;

	Assert(CacheMemoryContext != NULL);
	oldcxt = MemoryContextSwitchTo(CacheMemoryContext);

	if (fastpg_rust_catalog_relation_by_oid((uint32_t) cache->cc_reloid,
											&relation))
	{
		relname = relation.name;
		tupdesc = FastPgBuildRustCatalogTupleDesc(cache->cc_reloid);
	}
	else
	{
		MemoryContextSwitchTo(oldcxt);
		return false;
	}

	cache->cc_relname = pstrdup(relname);
	cache->cc_relisshared = false;

	for (i = 0; i < cache->cc_nkeys; ++i)
	{
		Oid			keytype;
		RegProcedure eqfunc;

		if (cache->cc_keyno[i] > 0)
		{
			Form_pg_attribute attr = TupleDescAttr(tupdesc,
												   cache->cc_keyno[i] - 1);

			keytype = attr->atttypid;
			Assert(attr->attnotnull);
		}
		else
		{
			if (cache->cc_keyno[i] < 0)
				elog(FATAL, "sys attributes are not supported in caches");
			keytype = OIDOID;
		}

		GetCCHashEqFuncs(keytype,
						 &cache->cc_hashfunc[i],
						 &eqfunc,
						 &cache->cc_fastequal[i]);
		fmgr_info_cxt(eqfunc,
					  &cache->cc_skey[i].sk_func,
					  CacheMemoryContext);
		cache->cc_skey[i].sk_attno = cache->cc_keyno[i];
		cache->cc_skey[i].sk_strategy = BTEqualStrategyNumber;
		cache->cc_skey[i].sk_subtype = InvalidOid;
		cache->cc_skey[i].sk_collation = C_COLLATION_OID;
	}

	cache->cc_tupdesc = tupdesc;
	MemoryContextSwitchTo(oldcxt);
	return true;
}

static HeapTuple
FastPgBuildNamespaceTuple(TupleDesc tupdesc,
						  const FastPgRustCatalogNamespace *namespace)
{
	Datum		values[Natts_pg_namespace] = {0};
	bool		nulls[Natts_pg_namespace] = {false};
	NameData	nspname;

	namestrcpy(&nspname, namespace->name);

	values[Anum_pg_namespace_oid - 1] = ObjectIdGetDatum((Oid) namespace->oid);
	values[Anum_pg_namespace_nspname - 1] = NameGetDatum(&nspname);
	values[Anum_pg_namespace_nspowner - 1] = ObjectIdGetDatum((Oid) namespace->owner_oid);
	nulls[Anum_pg_namespace_nspacl - 1] = true;

	return heap_form_tuple(tupdesc, values, nulls);
}

static uint64_t
FastPgCatalogScanBeginForKeys(CatCache *cache, int nkeys, Datum *arguments)
{
	int16_t		attnums[CATCACHE_MAXKEYS];
	uintptr_t	values[CATCACHE_MAXKEYS];
	size_t		filter_count = 0;

	if (nkeys <= 0 || arguments == NULL)
		return fastpg_rust_scan_begin((uint32_t) cache->cc_reloid);

	for (int index = 0; index < nkeys; index++)
	{
		int			attnum = cache->cc_keyno[index];

		if (attnum <= 0)
			continue;
		attnums[filter_count] = (int16_t) attnum;
		values[filter_count] = (uintptr_t) arguments[index];
		filter_count++;
	}

	if (filter_count == 0)
		return fastpg_rust_scan_begin((uint32_t) cache->cc_reloid);
	return fastpg_rust_scan_begin_filtered((uint32_t) cache->cc_reloid,
										   attnums,
										   values,
										   filter_count);
}

static bool
FastPgCatalogCacheHandlesMiss(CatCache *cache, int nkeys)
{
	if (fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) != 0)
		return true;

	if (FastPgCatalogCacheIsEmpty(cache))
		return true;

	if (cache->cc_reloid == RelationRelationId ||
		cache->cc_reloid == AttributeRelationId ||
		cache->cc_reloid == IndexRelationId ||
		cache->cc_reloid == ConstraintRelationId ||
		cache->cc_reloid == OperatorClassRelationId)
		return true;

	if (cache->cc_reloid == ProcedureRelationId ||
		cache->cc_reloid == NamespaceRelationId ||
		cache->cc_reloid == AggregateRelationId ||
		cache->cc_reloid == OperatorRelationId ||
		cache->cc_reloid == CastRelationId ||
		cache->cc_reloid == TypeRelationId)
		return true;

	if (cache->cc_reloid == StatisticRelationId &&
		nkeys == 3 &&
		cache->cc_keyno[0] == Anum_pg_statistic_starelid &&
		cache->cc_keyno[1] == Anum_pg_statistic_staattnum &&
		cache->cc_keyno[2] == Anum_pg_statistic_stainherit)
		return true;

	return false;
}

static bool
FastPgCatalogCacheIsEmpty(CatCache *cache)
{
	return fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) ==
		FASTPG_VIRTUAL_CATALOG_EMPTY;
}

static CatCList *
FastPgCatalogCacheBuildEmptyList(CatCache *cache, int nkeys,
								 Datum *arguments,
								 uint32 lHashValue,
								 dlist_head *lbucket)
{
	CatCList   *cl;
	MemoryContext oldcxt;

	oldcxt = MemoryContextSwitchTo(CacheMemoryContext);
	cl = (CatCList *)
		palloc(offsetof(CatCList, members));
	CatCacheCopyKeys(cache->cc_tupdesc, nkeys, cache->cc_keyno,
					 arguments, cl->keys);
	MemoryContextSwitchTo(oldcxt);

	cl->cl_magic = CL_MAGIC;
	cl->my_cache = cache;
	cl->refcount = 1;
	cl->dead = false;
	cl->ordered = true;
	cl->nkeys = nkeys;
	cl->hash_value = lHashValue;
	cl->n_members = 0;

	dlist_push_head(lbucket, &cl->cache_elem);
	cache->cc_nlist++;
	ResourceOwnerRememberCatCacheListRef(CurrentResourceOwner, cl);

	return cl;
}

static int
FastPgCompareOidAscending(Oid left, Oid right)
{
	if (left < right)
		return -1;
	if (left > right)
		return 1;
	return 0;
}

static int
FastPgCompareInt16Ascending(int16 left, int16 right)
{
	if (left < right)
		return -1;
	if (left > right)
		return 1;
	return 0;
}

static int
FastPgCompareAmopStrategyListMembers(const ListCell *left_cell,
									 const ListCell *right_cell)
{
	const CatCTup *left_tuple = (const CatCTup *) lfirst(left_cell);
	const CatCTup *right_tuple = (const CatCTup *) lfirst(right_cell);
	Form_pg_amop left = (Form_pg_amop) GETSTRUCT(&left_tuple->tuple);
	Form_pg_amop right = (Form_pg_amop) GETSTRUCT(&right_tuple->tuple);
	int			cmp;

	cmp = FastPgCompareOidAscending(left->amopfamily, right->amopfamily);
	if (cmp != 0)
		return cmp;
	cmp = FastPgCompareOidAscending(left->amoplefttype, right->amoplefttype);
	if (cmp != 0)
		return cmp;
	cmp = FastPgCompareOidAscending(left->amoprighttype, right->amoprighttype);
	if (cmp != 0)
		return cmp;
	return FastPgCompareInt16Ascending(left->amopstrategy, right->amopstrategy);
}

static int
FastPgCompareAmprocNumListMembers(const ListCell *left_cell,
								  const ListCell *right_cell)
{
	const CatCTup *left_tuple = (const CatCTup *) lfirst(left_cell);
	const CatCTup *right_tuple = (const CatCTup *) lfirst(right_cell);
	Form_pg_amproc left = (Form_pg_amproc) GETSTRUCT(&left_tuple->tuple);
	Form_pg_amproc right = (Form_pg_amproc) GETSTRUCT(&right_tuple->tuple);
	int			cmp;

	cmp = FastPgCompareOidAscending(left->amprocfamily, right->amprocfamily);
	if (cmp != 0)
		return cmp;
	cmp = FastPgCompareOidAscending(left->amproclefttype, right->amproclefttype);
	if (cmp != 0)
		return cmp;
	cmp = FastPgCompareOidAscending(left->amprocrighttype, right->amprocrighttype);
	if (cmp != 0)
		return cmp;
	return FastPgCompareInt16Ascending(left->amprocnum, right->amprocnum);
}

static bool
FastPgCatalogCacheSortListForOrdering(CatCache *cache, List *ctlist)
{
	if (cache->cc_reloid == AccessMethodOperatorRelationId &&
		cache->cc_indexoid == AccessMethodStrategyIndexId)
	{
		list_sort(ctlist, FastPgCompareAmopStrategyListMembers);
		return true;
	}
	if (cache->cc_reloid == AccessMethodProcedureRelationId &&
		cache->cc_indexoid == AccessMethodProcedureIndexId)
	{
		list_sort(ctlist, FastPgCompareAmprocNumListMembers);
		return true;
	}
	return false;
}

static CatCList *
FastPgCatalogCacheBuildList(CatCache *cache, int nkeys,
							Datum *arguments,
							uint32 lHashValue,
							dlist_head *lbucket)
{
	Datum	   *values;
	uint8_t    *isnull;
	uint64_t	scan_handle;
	uint64_t	row_id = 0;
	size_t		natts;
	List	   *ctlist = NIL;
	ListCell   *ctlist_item;
	CatCList   *cl;
	CatCTup    *ct;
	MemoryContext oldcxt;
	bool		ordered;
	int			nmembers;
	int			i = 0;

	if (fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) == 0)
		return NULL;
	if (cache->cc_tupdesc == NULL)
		return FastPgCatalogCacheBuildEmptyList(cache, nkeys, arguments,
												lHashValue, lbucket);

	natts = (size_t) cache->cc_tupdesc->natts;
	values = palloc0_array(Datum, natts);
	isnull = palloc0_array(uint8_t, natts);
	scan_handle = FastPgCatalogScanBeginForKeys(cache, nkeys, arguments);

	while (fastpg_rust_scan_next(scan_handle,
								 true,
								 (uintptr_t *) values,
								 isnull,
								 natts,
								 &row_id))
	{
		bool		matches = true;

		for (int key_index = 0; key_index < nkeys; key_index++)
		{
			int			attnum = cache->cc_keyno[key_index];

			if (attnum <= 0 || attnum > cache->cc_tupdesc->natts)
			{
				matches = false;
				break;
			}
			if (isnull[attnum - 1] != 0 ||
				!(cache->cc_fastequal[key_index]) (values[attnum - 1],
												   arguments[key_index]))
			{
				matches = false;
				break;
			}
		}

		if (matches)
		{
			bool	   *nulls = palloc0_array(bool, natts);
			HeapTuple	tuple;
			uint32		hashValue;
			Index		hashIndex;

			for (size_t index = 0; index < natts; index++)
				nulls[index] = isnull[index] != 0;
			tuple = heap_form_tuple(cache->cc_tupdesc, values, nulls);
			tuple->t_tableOid = cache->cc_reloid;
			if (!FastPgCatalogRowIdToTid(row_id, &tuple->t_self))
				elog(ERROR,
					 "fastpg catalog row id %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);

			hashValue = CatalogCacheComputeTupleHashValue(cache,
														  cache->cc_nkeys,
														  tuple);
			hashIndex = HASH_INDEX(hashValue, cache->cc_nbuckets);
			ct = CatalogCacheCreateEntry(cache, tuple, NULL,
										 hashValue, hashIndex);
			heap_freetuple(tuple);
			pfree(nulls);
			if (ct != NULL)
				ctlist = lappend(ctlist, ct);
		}
	}

	fastpg_rust_scan_end(scan_handle);
	pfree(values);
	pfree(isnull);
	ordered = FastPgCatalogCacheSortListForOrdering(cache, ctlist);

	oldcxt = MemoryContextSwitchTo(CacheMemoryContext);
	nmembers = list_length(ctlist);
	cl = (CatCList *)
		palloc(offsetof(CatCList, members) + nmembers * sizeof(CatCTup *));
	CatCacheCopyKeys(cache->cc_tupdesc, nkeys, cache->cc_keyno,
					 arguments, cl->keys);
	MemoryContextSwitchTo(oldcxt);

	cl->cl_magic = CL_MAGIC;
	cl->my_cache = cache;
	cl->refcount = 1;
	cl->dead = false;
	cl->ordered = ordered;
	cl->nkeys = nkeys;
	cl->hash_value = lHashValue;
	cl->n_members = nmembers;

	foreach(ctlist_item, ctlist)
	{
		ct = (CatCTup *) lfirst(ctlist_item);
		Assert(ct->c_list == NULL);
		ct->c_list = cl;
		cl->members[i++] = ct;
		if (ct->dead)
			cl->dead = true;
	}
	Assert(i == nmembers);

	dlist_push_head(lbucket, &cl->cache_elem);
	cache->cc_nlist++;
	ResourceOwnerRememberCatCacheListRef(CurrentResourceOwner, cl);

	return cl;
}

static HeapTuple
FastPgBuildClassTuple(TupleDesc tupdesc,
					  const FastPgRustCatalogRelation *relation)
{
	Datum		values[Natts_pg_class] = {0};
	bool		nulls[Natts_pg_class] = {false};
	NameData	relname;
	HeapTuple	tuple;
	uint32_t	rowtype_oid = InvalidOid;
	int32_t		relpages = 0;
	float4		reltuples = -1.0;

	namestrcpy(&relname, relation->name);
	fastpg_rust_catalog_relation_rowtype_oid_by_oid(relation->oid,
													&rowtype_oid);
	fastpg_rust_catalog_relation_planner_stats_by_oid(relation->oid,
													  &relpages,
													  &reltuples);

	values[Anum_pg_class_oid - 1] = ObjectIdGetDatum((Oid) relation->oid);
	values[Anum_pg_class_relname - 1] = NameGetDatum(&relname);
	values[Anum_pg_class_relnamespace - 1] = ObjectIdGetDatum((Oid) relation->namespace_oid);
	values[Anum_pg_class_reltype - 1] = ObjectIdGetDatum((Oid) rowtype_oid);
	values[Anum_pg_class_reloftype - 1] = ObjectIdGetDatum(InvalidOid);
	values[Anum_pg_class_relowner - 1] = ObjectIdGetDatum(BOOTSTRAP_SUPERUSERID);
	values[Anum_pg_class_relam - 1] =
		ObjectIdGetDatum(relation->relkind == RELKIND_INDEX ?
						 BTREE_AM_OID : HEAP_TABLE_AM_OID);
	values[Anum_pg_class_relfilenode - 1] = ObjectIdGetDatum((Oid) relation->oid);
	values[Anum_pg_class_reltablespace - 1] = ObjectIdGetDatum(InvalidOid);
	values[Anum_pg_class_relpages - 1] = Int32GetDatum(relpages);
	values[Anum_pg_class_reltuples - 1] = Float4GetDatum(reltuples);
	values[Anum_pg_class_relallvisible - 1] = Int32GetDatum(0);
	values[Anum_pg_class_relallfrozen - 1] = Int32GetDatum(0);
	values[Anum_pg_class_reltoastrelid - 1] = ObjectIdGetDatum(InvalidOid);
	values[Anum_pg_class_relhasindex - 1] =
		BoolGetDatum(relation->has_primary_key != 0);
	values[Anum_pg_class_relisshared - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relpersistence - 1] = CharGetDatum(RELPERSISTENCE_PERMANENT);
	values[Anum_pg_class_relkind - 1] = CharGetDatum((char) relation->relkind);
	values[Anum_pg_class_relnatts - 1] = Int16GetDatum((int16) relation->column_count);
	values[Anum_pg_class_relchecks - 1] = Int16GetDatum(0);
	values[Anum_pg_class_relhasrules - 1] =
		BoolGetDatum(relation->relkind == RELKIND_VIEW ||
					 relation->relkind == RELKIND_MATVIEW);
	values[Anum_pg_class_relhastriggers - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relhassubclass - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relrowsecurity - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relforcerowsecurity - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relispopulated - 1] = BoolGetDatum(true);
	values[Anum_pg_class_relreplident - 1] = CharGetDatum(REPLICA_IDENTITY_NOTHING);
	values[Anum_pg_class_relispartition - 1] = BoolGetDatum(false);
	values[Anum_pg_class_relrewrite - 1] = ObjectIdGetDatum(InvalidOid);
	values[Anum_pg_class_relfrozenxid - 1] = TransactionIdGetDatum(FirstNormalTransactionId);
	values[Anum_pg_class_relminmxid - 1] = MultiXactIdGetDatum(FirstMultiXactId);
	nulls[Anum_pg_class_relacl - 1] = true;
	nulls[Anum_pg_class_reloptions - 1] = true;
	nulls[Anum_pg_class_relpartbound - 1] = true;

	tuple = heap_form_tuple(tupdesc, values, nulls);
	tuple->t_tableOid = RelationRelationId;
	if (relation->row_id != 0 &&
		!FastPgCatalogRowIdToTid(relation->row_id, &tuple->t_self))
		elog(ERROR,
			 "fastpg catalog row id %llu cannot be represented as a CTID",
			 (unsigned long long) relation->row_id);
	return tuple;
}

static bool
FastPgOpclassForType(Oid type_oid, Oid *opclass_oid)
{
	uint32_t	fastpg_oid;

	if (!fastpg_rust_catalog_btree_opclass_for_type((uint32_t) type_oid,
													&fastpg_oid))
		return false;
	*opclass_oid = (Oid) fastpg_oid;
	return true;
}

static HeapTuple
FastPgBuildOpclassTuple(TupleDesc tupdesc, const FastPgRustCatalogOpclass *record)
{
	Datum		values[Natts_pg_opclass] = {0};
	bool		nulls[Natts_pg_opclass] = {false};
	NameData	opcname;

	namestrcpy(&opcname, record->name);

	values[Anum_pg_opclass_oid - 1] = ObjectIdGetDatum(record->oid);
	values[Anum_pg_opclass_opcmethod - 1] = ObjectIdGetDatum(record->method_oid);
	values[Anum_pg_opclass_opcname - 1] = NameGetDatum(&opcname);
	values[Anum_pg_opclass_opcnamespace - 1] = ObjectIdGetDatum(record->namespace_oid);
	values[Anum_pg_opclass_opcowner - 1] = ObjectIdGetDatum(record->owner_oid);
	values[Anum_pg_opclass_opcfamily - 1] = ObjectIdGetDatum(record->family_oid);
	values[Anum_pg_opclass_opcintype - 1] = ObjectIdGetDatum(record->input_type_oid);
	values[Anum_pg_opclass_opcdefault - 1] = BoolGetDatum(record->is_default != 0);
	values[Anum_pg_opclass_opckeytype - 1] = ObjectIdGetDatum(record->key_type_oid);

	return heap_form_tuple(tupdesc, values, nulls);
}

static HeapTuple
FastPgBuildIndexTuple(TupleDesc tupdesc,
					  const FastPgRustPrimaryKeyIndexInfo *index_info)
{
	HeapTuple	tuple;
	Datum		values[Natts_pg_index] = {0};
	bool		nulls[Natts_pg_index] = {false};
	int16		indkey_values[FASTPG_MAX_INDEX_KEYS] = {0};
	Oid			indcollation_values[FASTPG_MAX_INDEX_KEYS] = {0};
	Oid			indclass_values[FASTPG_MAX_INDEX_KEYS] = {0};
	int16		indoption_values[FASTPG_MAX_INDEX_KEYS] = {0};
	int			key_count = (int) index_info->key_count;

	if (key_count <= 0 || key_count > FASTPG_MAX_INDEX_KEYS)
		return NULL;

	for (int index = 0; index < key_count; index++)
	{
		Oid			opclass_oid;

		if (!FastPgOpclassForType((Oid) index_info->type_oids[index],
								  &opclass_oid))
			return NULL;
		indkey_values[index] = index_info->attnums[index];
		indcollation_values[index] = (Oid) index_info->collation_oids[index];
		indclass_values[index] = opclass_oid;
	}

	values[Anum_pg_index_indexrelid - 1] =
		ObjectIdGetDatum((Oid) index_info->index_oid);
	values[Anum_pg_index_indrelid - 1] =
		ObjectIdGetDatum((Oid) index_info->heap_oid);
	values[Anum_pg_index_indnatts - 1] = Int16GetDatum((int16) key_count);
	values[Anum_pg_index_indnkeyatts - 1] = Int16GetDatum((int16) key_count);
	values[Anum_pg_index_indisunique - 1] =
		BoolGetDatum(index_info->is_unique != 0);
	values[Anum_pg_index_indnullsnotdistinct - 1] =
		BoolGetDatum(index_info->nulls_not_distinct != 0);
	values[Anum_pg_index_indisprimary - 1] =
		BoolGetDatum(index_info->is_primary != 0);
	values[Anum_pg_index_indisexclusion - 1] = BoolGetDatum(false);
	values[Anum_pg_index_indimmediate - 1] =
		BoolGetDatum(index_info->is_immediate != 0);
	values[Anum_pg_index_indisclustered - 1] = BoolGetDatum(false);
	values[Anum_pg_index_indisvalid - 1] = BoolGetDatum(true);
	values[Anum_pg_index_indcheckxmin - 1] = BoolGetDatum(false);
	values[Anum_pg_index_indisready - 1] = BoolGetDatum(true);
	values[Anum_pg_index_indislive - 1] = BoolGetDatum(true);
	values[Anum_pg_index_indisreplident - 1] = BoolGetDatum(false);
	values[Anum_pg_index_indkey - 1] =
		PointerGetDatum(buildint2vector(indkey_values, key_count));
	values[Anum_pg_index_indcollation - 1] =
		PointerGetDatum(buildoidvector(indcollation_values, key_count));
	values[Anum_pg_index_indclass - 1] =
		PointerGetDatum(buildoidvector(indclass_values, key_count));
	values[Anum_pg_index_indoption - 1] =
		PointerGetDatum(buildint2vector(indoption_values, key_count));
	nulls[Anum_pg_index_indexprs - 1] = true;
	nulls[Anum_pg_index_indpred - 1] = true;

	tuple = heap_form_tuple(tupdesc, values, nulls);
	tuple->t_tableOid = IndexRelationId;
	if (index_info->row_id != 0 &&
		!FastPgCatalogRowIdToTid(index_info->row_id, &tuple->t_self))
		elog(ERROR,
			 "fastpg catalog row id %llu cannot be converted to a pg_index TID",
			 (unsigned long long) index_info->row_id);
	return tuple;
}

static HeapTuple
FastPgBuildAttributeTuple(TupleDesc tupdesc,
						  Oid relation_oid,
						  AttrNumber attnum,
						  const FastPgRustCatalogColumn *column)
{
	FastPgRustCatalogType type;
	HeapTuple	tuple;
	Datum		values[Natts_pg_attribute] = {0};
	bool		nulls[Natts_pg_attribute] = {false};
	NameData	attname;

	if (!fastpg_rust_catalog_type_by_oid(column->type_oid, &type))
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg catalog has no pg_type row for attribute type %u",
						column->type_oid)));

	namestrcpy(&attname, column->name);

	values[Anum_pg_attribute_attrelid - 1] = ObjectIdGetDatum(relation_oid);
	values[Anum_pg_attribute_attname - 1] = NameGetDatum(&attname);
	values[Anum_pg_attribute_atttypid - 1] = ObjectIdGetDatum((Oid) column->type_oid);
	values[Anum_pg_attribute_attlen - 1] = Int16GetDatum(column->attlen);
	values[Anum_pg_attribute_attnum - 1] = Int16GetDatum(attnum);
	values[Anum_pg_attribute_atttypmod - 1] = Int32GetDatum(column->type_mod);
	values[Anum_pg_attribute_attndims - 1] = Int16GetDatum(0);
	values[Anum_pg_attribute_attbyval - 1] = BoolGetDatum(column->attbyval != 0);
	values[Anum_pg_attribute_attalign - 1] = CharGetDatum((char) column->attalign);
	values[Anum_pg_attribute_attstorage - 1] = CharGetDatum((char) column->attstorage);
	values[Anum_pg_attribute_attcompression - 1] = CharGetDatum('\0');
	values[Anum_pg_attribute_attnotnull - 1] = BoolGetDatum(column->is_not_null != 0);
	values[Anum_pg_attribute_atthasdef - 1] = BoolGetDatum(column->has_default != 0);
	values[Anum_pg_attribute_atthasmissing - 1] = BoolGetDatum(false);
	values[Anum_pg_attribute_attidentity - 1] = CharGetDatum('\0');
	values[Anum_pg_attribute_attgenerated - 1] = CharGetDatum((char) column->generated);
	values[Anum_pg_attribute_attisdropped - 1] = BoolGetDatum(column->is_dropped != 0);
	values[Anum_pg_attribute_attislocal - 1] = BoolGetDatum(true);
	values[Anum_pg_attribute_attinhcount - 1] = Int16GetDatum(0);
	values[Anum_pg_attribute_attcollation - 1] =
		ObjectIdGetDatum((Oid) column->attcollation);
	values[Anum_pg_attribute_attstattarget - 1] = Int16GetDatum(-1);
	nulls[Anum_pg_attribute_attacl - 1] = true;
	nulls[Anum_pg_attribute_attoptions - 1] = true;
	nulls[Anum_pg_attribute_attfdwoptions - 1] = true;
	nulls[Anum_pg_attribute_attmissingval - 1] = true;

	tuple = heap_form_tuple(tupdesc, values, nulls);
	tuple->t_tableOid = AttributeRelationId;
	if (column->row_id != 0 &&
		!FastPgCatalogRowIdToTid(column->row_id, &tuple->t_self))
		elog(ERROR,
			 "fastpg catalog row id %llu cannot be converted to a pg_attribute TID",
			 (unsigned long long) column->row_id);
	return tuple;
}

static HeapTuple
FastPgBuildSystemAttributeTuple(TupleDesc tupdesc,
								Oid relation_oid,
								const FormData_pg_attribute *attribute)
{
	Datum		values[Natts_pg_attribute] = {0};
	bool		nulls[Natts_pg_attribute] = {false};
	HeapTuple	tuple;

	values[Anum_pg_attribute_attrelid - 1] = ObjectIdGetDatum(relation_oid);
	values[Anum_pg_attribute_attname - 1] = NameGetDatum(&attribute->attname);
	values[Anum_pg_attribute_atttypid - 1] = ObjectIdGetDatum(attribute->atttypid);
	values[Anum_pg_attribute_attlen - 1] = Int16GetDatum(attribute->attlen);
	values[Anum_pg_attribute_attnum - 1] = Int16GetDatum(attribute->attnum);
	values[Anum_pg_attribute_atttypmod - 1] = Int32GetDatum(attribute->atttypmod);
	values[Anum_pg_attribute_attndims - 1] = Int16GetDatum(attribute->attndims);
	values[Anum_pg_attribute_attbyval - 1] = BoolGetDatum(attribute->attbyval);
	values[Anum_pg_attribute_attalign - 1] = CharGetDatum(attribute->attalign);
	values[Anum_pg_attribute_attstorage - 1] = CharGetDatum(attribute->attstorage);
	values[Anum_pg_attribute_attcompression - 1] =
		CharGetDatum(attribute->attcompression);
	values[Anum_pg_attribute_attnotnull - 1] =
		BoolGetDatum(attribute->attnotnull);
	values[Anum_pg_attribute_atthasdef - 1] = BoolGetDatum(false);
	values[Anum_pg_attribute_atthasmissing - 1] = BoolGetDatum(false);
	values[Anum_pg_attribute_attidentity - 1] = CharGetDatum('\0');
	values[Anum_pg_attribute_attgenerated - 1] = CharGetDatum('\0');
	values[Anum_pg_attribute_attisdropped - 1] =
		BoolGetDatum(attribute->attisdropped);
	values[Anum_pg_attribute_attislocal - 1] =
		BoolGetDatum(attribute->attislocal);
	values[Anum_pg_attribute_attinhcount - 1] =
		Int16GetDatum(attribute->attinhcount);
	values[Anum_pg_attribute_attcollation - 1] =
		ObjectIdGetDatum(attribute->attcollation);
	values[Anum_pg_attribute_attstattarget - 1] = Int16GetDatum(-1);
	nulls[Anum_pg_attribute_attacl - 1] = true;
	nulls[Anum_pg_attribute_attoptions - 1] = true;
	nulls[Anum_pg_attribute_attfdwoptions - 1] = true;
	nulls[Anum_pg_attribute_attmissingval - 1] = true;

	tuple = heap_form_tuple(tupdesc, values, nulls);
	tuple->t_tableOid = AttributeRelationId;
	return tuple;
}

static HeapTuple
FastPgBuildTypeTuple(TupleDesc tupdesc, const FastPgRustCatalogType *type)
{
	Datum		values[Natts_pg_type] = {0};
	bool		nulls[Natts_pg_type] = {false};
	NameData	typname;
	HeapTuple	tuple;

	namestrcpy(&typname, type->name);

	values[Anum_pg_type_oid - 1] = ObjectIdGetDatum((Oid) type->oid);
	values[Anum_pg_type_typname - 1] = NameGetDatum(&typname);
	values[Anum_pg_type_typnamespace - 1] = ObjectIdGetDatum((Oid) type->namespace_oid);
	values[Anum_pg_type_typowner - 1] = ObjectIdGetDatum((Oid) type->owner_oid);
	values[Anum_pg_type_typlen - 1] = Int16GetDatum((int16) type->typlen);
	values[Anum_pg_type_typbyval - 1] = BoolGetDatum(type->typbyval != 0);
	values[Anum_pg_type_typtype - 1] = CharGetDatum((char) type->typtype);
	values[Anum_pg_type_typcategory - 1] = CharGetDatum((char) type->typcategory);
	values[Anum_pg_type_typispreferred - 1] = BoolGetDatum(type->typispreferred != 0);
	values[Anum_pg_type_typisdefined - 1] = BoolGetDatum(type->typisdefined != 0);
	values[Anum_pg_type_typdelim - 1] = CharGetDatum((char) type->typdelim);
	values[Anum_pg_type_typrelid - 1] = ObjectIdGetDatum((Oid) type->typrelid);
	values[Anum_pg_type_typsubscript - 1] = ObjectIdGetDatum((Oid) type->typsubscript);
	values[Anum_pg_type_typelem - 1] = ObjectIdGetDatum((Oid) type->typelem);
	values[Anum_pg_type_typarray - 1] = ObjectIdGetDatum((Oid) type->typarray);
	values[Anum_pg_type_typinput - 1] = ObjectIdGetDatum((Oid) type->typinput);
	values[Anum_pg_type_typoutput - 1] = ObjectIdGetDatum((Oid) type->typoutput);
	values[Anum_pg_type_typreceive - 1] = ObjectIdGetDatum((Oid) type->typreceive);
	values[Anum_pg_type_typsend - 1] = ObjectIdGetDatum((Oid) type->typsend);
	values[Anum_pg_type_typmodin - 1] = ObjectIdGetDatum((Oid) type->typmodin);
	values[Anum_pg_type_typmodout - 1] = ObjectIdGetDatum((Oid) type->typmodout);
	values[Anum_pg_type_typanalyze - 1] = ObjectIdGetDatum(InvalidOid);
	values[Anum_pg_type_typalign - 1] = CharGetDatum((char) type->typalign);
	values[Anum_pg_type_typstorage - 1] = CharGetDatum((char) type->typstorage);
	values[Anum_pg_type_typnotnull - 1] = BoolGetDatum(false);
	values[Anum_pg_type_typbasetype - 1] = ObjectIdGetDatum((Oid) type->typbasetype);
	values[Anum_pg_type_typtypmod - 1] = Int32GetDatum(type->typtypmod);
	values[Anum_pg_type_typndims - 1] = Int32GetDatum(0);
	values[Anum_pg_type_typcollation - 1] = ObjectIdGetDatum((Oid) type->typcollation);
	nulls[Anum_pg_type_typdefaultbin - 1] = true;
	nulls[Anum_pg_type_typdefault - 1] = true;
	nulls[Anum_pg_type_typacl - 1] = true;

	tuple = heap_form_tuple(tupdesc, values, nulls);
	tuple->t_tableOid = TypeRelationId;
	if (type->row_id != 0 &&
		!FastPgCatalogRowIdToTid(type->row_id, &tuple->t_self))
		elog(ERROR,
			 "fastpg catalog row id %llu cannot be represented as a CTID",
			 (unsigned long long) type->row_id);
	return tuple;
}

static HeapTuple
FastPgBuildProcTuple(TupleDesc tupdesc, const FastPgRustCatalogProc *proc)
{
	Datum		values[Natts_pg_proc] = {0};
	bool		nulls[Natts_pg_proc] = {false};
	NameData	proname;
	Oid			argtypes[FASTPG_PROC_MAX_ARGS];
	oidvector  *proargtypes;

	namestrcpy(&proname, proc->name);
	for (int i = 0; i < proc->arg_count; i++)
		argtypes[i] = (Oid) proc->arg_type_oids[i];
	proargtypes = buildoidvector(argtypes, proc->arg_count);

	values[Anum_pg_proc_oid - 1] = ObjectIdGetDatum((Oid) proc->oid);
	values[Anum_pg_proc_proname - 1] = NameGetDatum(&proname);
	values[Anum_pg_proc_pronamespace - 1] = ObjectIdGetDatum((Oid) proc->namespace_oid);
	values[Anum_pg_proc_proowner - 1] = ObjectIdGetDatum((Oid) proc->owner_oid);
	values[Anum_pg_proc_prolang - 1] = ObjectIdGetDatum((Oid) proc->language_oid);
	values[Anum_pg_proc_procost - 1] = Float4GetDatum(proc->cost);
	values[Anum_pg_proc_prorows - 1] = Float4GetDatum(proc->rows);
	values[Anum_pg_proc_provariadic - 1] = ObjectIdGetDatum((Oid) proc->variadic_oid);
	values[Anum_pg_proc_prosupport - 1] = ObjectIdGetDatum((Oid) proc->support_oid);
	values[Anum_pg_proc_prokind - 1] = CharGetDatum((char) proc->kind);
	values[Anum_pg_proc_prosecdef - 1] = BoolGetDatum(proc->security_definer != 0);
	values[Anum_pg_proc_proleakproof - 1] = BoolGetDatum(proc->leakproof != 0);
	values[Anum_pg_proc_proisstrict - 1] = BoolGetDatum(proc->is_strict != 0);
	values[Anum_pg_proc_proretset - 1] = BoolGetDatum(proc->returns_set != 0);
	values[Anum_pg_proc_provolatile - 1] = CharGetDatum((char) proc->volatility);
	values[Anum_pg_proc_proparallel - 1] = CharGetDatum((char) proc->parallel);
	values[Anum_pg_proc_pronargs - 1] = Int16GetDatum((int16) proc->arg_count);
	values[Anum_pg_proc_pronargdefaults - 1] = Int16GetDatum((int16) proc->arg_default_count);
	values[Anum_pg_proc_prorettype - 1] = ObjectIdGetDatum((Oid) proc->return_type_oid);
	values[Anum_pg_proc_proargtypes - 1] = PointerGetDatum(proargtypes);
	nulls[Anum_pg_proc_proallargtypes - 1] = true;
	nulls[Anum_pg_proc_proargmodes - 1] = true;
	nulls[Anum_pg_proc_proargnames - 1] = true;
	nulls[Anum_pg_proc_proargdefaults - 1] = true;
	nulls[Anum_pg_proc_protrftypes - 1] = true;
	values[Anum_pg_proc_prosrc - 1] = CStringGetTextDatum(proc->source);
	nulls[Anum_pg_proc_probin - 1] = true;
	nulls[Anum_pg_proc_prosqlbody - 1] = true;
	nulls[Anum_pg_proc_proconfig - 1] = true;
	nulls[Anum_pg_proc_proacl - 1] = true;

	return heap_form_tuple(tupdesc, values, nulls);
}

static HeapTuple
FastPgBuildAggregateTuple(TupleDesc tupdesc, const FastPgRustCatalogAggregate *agg)
{
	Datum		values[Natts_pg_aggregate] = {0};
	bool		nulls[Natts_pg_aggregate] = {false};
	char		init_value[FASTPG_PROC_SOURCE_LEN];
	char		moving_init_value[FASTPG_PROC_SOURCE_LEN];

	values[Anum_pg_aggregate_aggfnoid - 1] = ObjectIdGetDatum((Oid) agg->function_oid);
	values[Anum_pg_aggregate_aggkind - 1] = CharGetDatum((char) agg->kind);
	values[Anum_pg_aggregate_aggnumdirectargs - 1] = Int16GetDatum((int16) agg->direct_arg_count);
	values[Anum_pg_aggregate_aggtransfn - 1] = ObjectIdGetDatum((Oid) agg->transition_fn_oid);
	values[Anum_pg_aggregate_aggfinalfn - 1] = ObjectIdGetDatum((Oid) agg->final_fn_oid);
	values[Anum_pg_aggregate_aggcombinefn - 1] = ObjectIdGetDatum((Oid) agg->combine_fn_oid);
	values[Anum_pg_aggregate_aggserialfn - 1] = ObjectIdGetDatum((Oid) agg->serial_fn_oid);
	values[Anum_pg_aggregate_aggdeserialfn - 1] = ObjectIdGetDatum((Oid) agg->deserial_fn_oid);
	values[Anum_pg_aggregate_aggmtransfn - 1] = ObjectIdGetDatum((Oid) agg->moving_transition_fn_oid);
	values[Anum_pg_aggregate_aggminvtransfn - 1] = ObjectIdGetDatum((Oid) agg->moving_inverse_fn_oid);
	values[Anum_pg_aggregate_aggmfinalfn - 1] = ObjectIdGetDatum((Oid) agg->moving_final_fn_oid);
	values[Anum_pg_aggregate_aggfinalextra - 1] = BoolGetDatum(agg->final_extra != 0);
	values[Anum_pg_aggregate_aggmfinalextra - 1] = BoolGetDatum(agg->moving_final_extra != 0);
	values[Anum_pg_aggregate_aggfinalmodify - 1] = CharGetDatum((char) agg->final_modify);
	values[Anum_pg_aggregate_aggmfinalmodify - 1] = CharGetDatum((char) agg->moving_final_modify);
	values[Anum_pg_aggregate_aggsortop - 1] = ObjectIdGetDatum((Oid) agg->sort_operator_oid);
	values[Anum_pg_aggregate_aggtranstype - 1] = ObjectIdGetDatum((Oid) agg->transition_type_oid);
	values[Anum_pg_aggregate_aggtransspace - 1] = Int32GetDatum(agg->transition_space);
	values[Anum_pg_aggregate_aggmtranstype - 1] = ObjectIdGetDatum((Oid) agg->moving_transition_type_oid);
	values[Anum_pg_aggregate_aggmtransspace - 1] = Int32GetDatum(agg->moving_transition_space);

	if (agg->has_init_value &&
		fastpg_rust_catalog_aggregate_init_value(agg->function_oid, false,
												 init_value, sizeof(init_value)))
		values[Anum_pg_aggregate_agginitval - 1] = CStringGetTextDatum(init_value);
	else
		nulls[Anum_pg_aggregate_agginitval - 1] = true;

	if (agg->has_moving_init_value &&
		fastpg_rust_catalog_aggregate_init_value(agg->function_oid, true,
												 moving_init_value,
												 sizeof(moving_init_value)))
		values[Anum_pg_aggregate_aggminitval - 1] = CStringGetTextDatum(moving_init_value);
	else
		nulls[Anum_pg_aggregate_aggminitval - 1] = true;

	return heap_form_tuple(tupdesc, values, nulls);
}

static HeapTuple
FastPgBuildOperatorTuple(TupleDesc tupdesc, const FastPgRustCatalogOperator *oper)
{
	Datum		values[Natts_pg_operator] = {0};
	bool		nulls[Natts_pg_operator] = {false};
	NameData	oprname;

	namestrcpy(&oprname, oper->name);

	values[Anum_pg_operator_oid - 1] = ObjectIdGetDatum((Oid) oper->oid);
	values[Anum_pg_operator_oprname - 1] = NameGetDatum(&oprname);
	values[Anum_pg_operator_oprnamespace - 1] = ObjectIdGetDatum((Oid) oper->namespace_oid);
	values[Anum_pg_operator_oprowner - 1] = ObjectIdGetDatum((Oid) oper->owner_oid);
	values[Anum_pg_operator_oprkind - 1] = CharGetDatum((char) oper->kind);
	values[Anum_pg_operator_oprcanmerge - 1] = BoolGetDatum(oper->can_merge != 0);
	values[Anum_pg_operator_oprcanhash - 1] = BoolGetDatum(oper->can_hash != 0);
	values[Anum_pg_operator_oprleft - 1] = ObjectIdGetDatum((Oid) oper->left_type_oid);
	values[Anum_pg_operator_oprright - 1] = ObjectIdGetDatum((Oid) oper->right_type_oid);
	values[Anum_pg_operator_oprresult - 1] = ObjectIdGetDatum((Oid) oper->result_type_oid);
	values[Anum_pg_operator_oprcom - 1] = ObjectIdGetDatum((Oid) oper->commutator_oid);
	values[Anum_pg_operator_oprnegate - 1] = ObjectIdGetDatum((Oid) oper->negator_oid);
	values[Anum_pg_operator_oprcode - 1] = ObjectIdGetDatum((Oid) oper->code_fn_oid);
	values[Anum_pg_operator_oprrest - 1] = ObjectIdGetDatum((Oid) oper->rest_fn_oid);
	values[Anum_pg_operator_oprjoin - 1] = ObjectIdGetDatum((Oid) oper->join_fn_oid);

	return heap_form_tuple(tupdesc, values, nulls);
}

static HeapTuple
FastPgBuildCastTuple(TupleDesc tupdesc, const FastPgRustCatalogCast *cast)
{
	Datum		values[Natts_pg_cast] = {0};
	bool		nulls[Natts_pg_cast] = {false};

	values[Anum_pg_cast_oid - 1] = ObjectIdGetDatum((Oid) cast->oid);
	values[Anum_pg_cast_castsource - 1] = ObjectIdGetDatum((Oid) cast->source_type_oid);
	values[Anum_pg_cast_casttarget - 1] = ObjectIdGetDatum((Oid) cast->target_type_oid);
	values[Anum_pg_cast_castfunc - 1] = ObjectIdGetDatum((Oid) cast->function_oid);
	values[Anum_pg_cast_castcontext - 1] = CharGetDatum((char) cast->context);
	values[Anum_pg_cast_castmethod - 1] = CharGetDatum((char) cast->method);

	return heap_form_tuple(tupdesc, values, nulls);
}

static bool
FastPgCatalogRowIdToTid(uint64_t row_id, ItemPointer tid)
{
	uint64_t	zero_index;
	uint64_t	block;
	OffsetNumber offset;

	if (row_id == 0)
		return false;

	zero_index = row_id - 1;
	block = zero_index / (uint64_t) MaxOffsetNumber;
	if (block > UINT32_MAX)
		return false;

	offset = (OffsetNumber) (zero_index % (uint64_t) MaxOffsetNumber) +
		FirstOffsetNumber;
	ItemPointerSet(tid, (BlockNumber) block, offset);
	return true;
}

static HeapTuple
FastPgCatalogCacheLookupGeneric(CatCache *cache, int nkeys, Datum *arguments)
{
	Datum	   *values;
	uint8_t    *isnull;
	uint64_t	scan_handle;
	uint64_t	row_id = 0;
	size_t		natts;
	HeapTuple	tuple = NULL;

	if (fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) == 0)
		return NULL;
	if (cache->cc_tupdesc == NULL)
		return NULL;

	natts = (size_t) cache->cc_tupdesc->natts;
	values = palloc0_array(Datum, natts);
	isnull = palloc0_array(uint8_t, natts);
	scan_handle = FastPgCatalogScanBeginForKeys(cache, nkeys, arguments);

	while (fastpg_rust_scan_next(scan_handle,
								 true,
								 (uintptr_t *) values,
								 isnull,
								 natts,
								 &row_id))
	{
		bool		matches = true;

		for (int key_index = 0; key_index < nkeys; key_index++)
		{
			int			attnum = cache->cc_keyno[key_index];

			if (attnum <= 0 || attnum > cache->cc_tupdesc->natts)
			{
				matches = false;
				break;
			}
			if (isnull[attnum - 1] != 0 ||
				!(cache->cc_fastequal[key_index]) (values[attnum - 1],
												   arguments[key_index]))
			{
				matches = false;
				break;
			}
		}

		if (matches)
		{
			bool	   *nulls = palloc0_array(bool, natts);

			for (size_t index = 0; index < natts; index++)
				nulls[index] = isnull[index] != 0;
			tuple = heap_form_tuple(cache->cc_tupdesc, values, nulls);
			tuple->t_tableOid = cache->cc_reloid;
			if (!FastPgCatalogRowIdToTid(row_id, &tuple->t_self))
				elog(ERROR,
					 "fastpg catalog row id %llu cannot be represented as a CTID",
					 (unsigned long long) row_id);
			break;
		}
	}

	fastpg_rust_scan_end(scan_handle);
	pfree(values);
	pfree(isnull);
	return tuple;
}

static HeapTuple
FastPgCatalogCacheLookup(CatCache *cache, int nkeys, Datum *arguments)
{
	if (cache->cc_reloid == RelationRelationId &&
		fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) != 0)
	{
		HeapTuple	tuple;

		tuple = FastPgCatalogCacheLookupGeneric(cache, nkeys, arguments);
		if (tuple != NULL)
			return tuple;
	}

	if (cache->cc_reloid == RelationRelationId)
	{
		FastPgRustCatalogRelation relation;

		if (nkeys == 1 && cache->cc_keyno[0] == Anum_pg_class_oid)
		{
			Oid			relation_oid = DatumGetObjectId(arguments[0]);

			if (fastpg_rust_catalog_relation_by_oid((uint32_t) relation_oid,
													&relation))
				return FastPgBuildClassTuple(cache->cc_tupdesc, &relation);
		}
		else if (nkeys == 2 &&
				 cache->cc_keyno[0] == Anum_pg_class_relname &&
				 cache->cc_keyno[1] == Anum_pg_class_relnamespace)
		{
			const char *relation_name = DatumGetCString(arguments[0]);
			Oid			namespace_oid = DatumGetObjectId(arguments[1]);

			if (fastpg_rust_catalog_relation_by_name(relation_name,
												 (uint32_t) namespace_oid,
												 &relation))
				return FastPgBuildClassTuple(cache->cc_tupdesc, &relation);
		}
	}
	else if (cache->cc_reloid == TypeRelationId)
	{
		FastPgRustCatalogType type;

		if (nkeys == 1 && cache->cc_keyno[0] == Anum_pg_type_oid)
		{
			Oid			type_oid = DatumGetObjectId(arguments[0]);

			if (fastpg_rust_catalog_type_by_oid((uint32_t) type_oid, &type))
				return FastPgBuildTypeTuple(cache->cc_tupdesc, &type);
		}
		else if (nkeys == 2 &&
				 cache->cc_keyno[0] == Anum_pg_type_typname &&
				 cache->cc_keyno[1] == Anum_pg_type_typnamespace)
		{
			const char *typname = DatumGetCString(arguments[0]);
			Oid			namespace_oid = DatumGetObjectId(arguments[1]);

			if (fastpg_rust_catalog_type_by_name(typname,
												 (uint32_t) namespace_oid,
												 &type))
				return FastPgBuildTypeTuple(cache->cc_tupdesc, &type);
		}
	}
	else if (cache->cc_reloid == AttributeRelationId)
	{
		FastPgRustCatalogRelation relation;

		if (nkeys == 2 &&
			cache->cc_keyno[0] == Anum_pg_attribute_attrelid &&
			cache->cc_keyno[1] == Anum_pg_attribute_attnum)
		{
			Oid			relation_oid = DatumGetObjectId(arguments[0]);
			AttrNumber	attnum = DatumGetInt16(arguments[1]);

			if (attnum < 0 && attnum >= TableOidAttributeNumber &&
				fastpg_rust_catalog_relation_by_oid((uint32_t) relation_oid,
													&relation))
				return FastPgBuildSystemAttributeTuple(cache->cc_tupdesc,
													   relation_oid,
													   SystemAttributeDefinition(attnum));
		}
		else if (nkeys == 2 &&
				 cache->cc_keyno[0] == Anum_pg_attribute_attrelid &&
				 cache->cc_keyno[1] == Anum_pg_attribute_attname)
		{
			Oid			relation_oid = DatumGetObjectId(arguments[0]);
			const char *attribute_name = DatumGetCString(arguments[1]);
			const FormData_pg_attribute *attribute =
				SystemAttributeByName(attribute_name);

			if (attribute != NULL &&
				fastpg_rust_catalog_relation_by_oid((uint32_t) relation_oid,
													&relation))
				return FastPgBuildSystemAttributeTuple(cache->cc_tupdesc,
													   relation_oid,
													   attribute);
		}
	}

	if (fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid) != 0)
		return FastPgCatalogCacheLookupGeneric(cache, nkeys, arguments);

	if (cache->cc_reloid == AttributeRelationId)
	{
		FastPgRustCatalogColumn column;
		Oid			relation_oid;
		FastPgRustCatalogRelation relation;

		if (nkeys != 2)
			return NULL;
		relation_oid = DatumGetObjectId(arguments[0]);
		if (!fastpg_rust_catalog_relation_by_oid((uint32_t) relation_oid,
												 &relation))
			return NULL;

		if (cache->cc_keyno[0] == Anum_pg_attribute_attrelid &&
			cache->cc_keyno[1] == Anum_pg_attribute_attnum)
		{
			AttrNumber	attnum = DatumGetInt16(arguments[1]);

			if (attnum <= 0 || attnum > relation.column_count)
				return NULL;
			if (fastpg_rust_catalog_relation_column_by_index((uint32_t) relation_oid,
															 (size_t) (attnum - 1),
															 &column))
				return FastPgBuildAttributeTuple(cache->cc_tupdesc,
												 relation_oid,
												 attnum,
												 &column);
		}
		else if (cache->cc_keyno[0] == Anum_pg_attribute_attrelid &&
				 cache->cc_keyno[1] == Anum_pg_attribute_attname)
		{
			const char *attribute_name = DatumGetCString(arguments[1]);

			for (uint16_t i = 0; i < relation.column_count; i++)
			{
				if (!fastpg_rust_catalog_relation_column_by_index((uint32_t) relation_oid,
																  (size_t) i,
																  &column))
					continue;
				if (strncmp(column.name, attribute_name, NAMEDATALEN) == 0)
				return FastPgBuildAttributeTuple(cache->cc_tupdesc,
												 relation_oid,
												 (AttrNumber) (i + 1),
												 &column);
			}
		}
	}
	else if (cache->cc_reloid == IndexRelationId)
	{
		FastPgRustPrimaryKeyIndexInfo index_info;

		if (nkeys == 1 &&
			cache->cc_keyno[0] == Anum_pg_index_indexrelid &&
			fastpg_rust_catalog_primary_key_index_info((uint32_t) DatumGetObjectId(arguments[0]),
													   &index_info))
			return FastPgBuildIndexTuple(cache->cc_tupdesc, &index_info);
	}
	else if (cache->cc_reloid == OperatorClassRelationId)
	{
		FastPgRustCatalogOpclass opclass;
		bool		found = false;

		if (nkeys == 1 && cache->cc_keyno[0] == Anum_pg_opclass_oid)
		{
			found = fastpg_rust_catalog_opclass_by_oid((uint32_t) DatumGetObjectId(arguments[0]),
													   &opclass);
		}
		else if (nkeys == 3 &&
				 cache->cc_keyno[0] == Anum_pg_opclass_opcmethod &&
				 cache->cc_keyno[1] == Anum_pg_opclass_opcname &&
				 cache->cc_keyno[2] == Anum_pg_opclass_opcnamespace)
		{
			found = fastpg_rust_catalog_opclass_by_name((uint32_t) DatumGetObjectId(arguments[0]),
														DatumGetCString(arguments[1]),
														(uint32_t) DatumGetObjectId(arguments[2]),
														&opclass);
		}

		if (found)
			return FastPgBuildOpclassTuple(cache->cc_tupdesc, &opclass);
	}
	else if (cache->cc_reloid == ProcedureRelationId)
	{
		FastPgRustCatalogProc proc;
		Oid			proc_oid = DatumGetObjectId(arguments[0]);

		if (nkeys != 1)
			return NULL;
		if (fastpg_rust_catalog_proc_by_oid((uint32_t) proc_oid, &proc))
			return FastPgBuildProcTuple(cache->cc_tupdesc, &proc);
	}
	else if (cache->cc_reloid == NamespaceRelationId)
	{
		FastPgRustCatalogNamespace namespace;

		if (nkeys != 1)
			return NULL;
		if (cache->cc_keyno[0] == Anum_pg_namespace_oid)
		{
			Oid			namespace_oid = DatumGetObjectId(arguments[0]);

			if (fastpg_rust_catalog_namespace_by_oid((uint32_t) namespace_oid,
													 &namespace))
				return FastPgBuildNamespaceTuple(cache->cc_tupdesc, &namespace);
		}
		else if (cache->cc_keyno[0] == Anum_pg_namespace_nspname)
		{
			const char *namespace_name = DatumGetCString(arguments[0]);

			if (fastpg_rust_catalog_namespace_by_name(namespace_name,
													  &namespace))
				return FastPgBuildNamespaceTuple(cache->cc_tupdesc, &namespace);
		}
	}
	else if (cache->cc_reloid == AggregateRelationId)
	{
		FastPgRustCatalogAggregate agg;
		Oid			func_oid = DatumGetObjectId(arguments[0]);

		if (nkeys != 1)
			return NULL;
		if (fastpg_rust_catalog_aggregate_by_proc_oid((uint32_t) func_oid, &agg))
			return FastPgBuildAggregateTuple(cache->cc_tupdesc, &agg);
	}
	else if (cache->cc_reloid == OperatorRelationId)
	{
		FastPgRustCatalogOperator oper;

		if (nkeys == 1 && cache->cc_keyno[0] == Anum_pg_operator_oid)
		{
			Oid			oper_oid = DatumGetObjectId(arguments[0]);

			if (fastpg_rust_catalog_operator_by_oid((uint32_t) oper_oid, &oper))
				return FastPgBuildOperatorTuple(cache->cc_tupdesc, &oper);
		}
		else if (nkeys == 4 &&
				 cache->cc_keyno[0] == Anum_pg_operator_oprname &&
				 cache->cc_keyno[1] == Anum_pg_operator_oprleft &&
				 cache->cc_keyno[2] == Anum_pg_operator_oprright &&
				 cache->cc_keyno[3] == Anum_pg_operator_oprnamespace)
		{
			const char *opername = DatumGetCString(arguments[0]);
			Oid			oprleft = DatumGetObjectId(arguments[1]);
			Oid			oprright = DatumGetObjectId(arguments[2]);
			Oid			namespace_oid = DatumGetObjectId(arguments[3]);

			if (fastpg_rust_catalog_operator_by_signature(opername,
														  (uint32_t) oprleft,
														  (uint32_t) oprright,
														  (uint32_t) namespace_oid,
														  &oper))
				return FastPgBuildOperatorTuple(cache->cc_tupdesc, &oper);
		}
	}
	else if (cache->cc_reloid == CastRelationId)
	{
		FastPgRustCatalogCast cast;

		if (nkeys == 2 &&
			cache->cc_keyno[0] == Anum_pg_cast_castsource &&
			cache->cc_keyno[1] == Anum_pg_cast_casttarget)
		{
			Oid			source_type = DatumGetObjectId(arguments[0]);
			Oid			target_type = DatumGetObjectId(arguments[1]);

			if (fastpg_rust_catalog_cast_by_source_target((uint32_t) source_type,
														  (uint32_t) target_type,
														  &cast))
				return FastPgBuildCastTuple(cache->cc_tupdesc, &cast);
		}
	}
	return FastPgCatalogCacheLookupGeneric(cache, nkeys, arguments);
}
#endif

/*
 *		CatalogCacheComputeHashValue
 *
 * Compute the hash value associated with a given set of lookup keys
 */
static uint32
CatalogCacheComputeHashValue(CatCache *cache, int nkeys,
							 Datum v1, Datum v2, Datum v3, Datum v4)
{
	uint32		hashValue = 0;
	uint32		oneHash;
	CCHashFN   *cc_hashfunc = cache->cc_hashfunc;

	CACHE_elog(DEBUG2, "CatalogCacheComputeHashValue %s %d %p",
			   cache->cc_relname, nkeys, cache);

	switch (nkeys)
	{
		case 4:
			oneHash = (cc_hashfunc[3]) (v4);
			hashValue ^= pg_rotate_left32(oneHash, 24);
			pg_fallthrough;
		case 3:
			oneHash = (cc_hashfunc[2]) (v3);
			hashValue ^= pg_rotate_left32(oneHash, 16);
			pg_fallthrough;
		case 2:
			oneHash = (cc_hashfunc[1]) (v2);
			hashValue ^= pg_rotate_left32(oneHash, 8);
			pg_fallthrough;
		case 1:
			oneHash = (cc_hashfunc[0]) (v1);
			hashValue ^= oneHash;
			break;
		default:
			elog(FATAL, "wrong number of hash keys: %d", nkeys);
			break;
	}

	return hashValue;
}

/*
 *		CatalogCacheComputeTupleHashValue
 *
 * Compute the hash value associated with a given tuple to be cached
 */
static uint32
CatalogCacheComputeTupleHashValue(CatCache *cache, int nkeys, HeapTuple tuple)
{
	Datum		v1 = 0,
				v2 = 0,
				v3 = 0,
				v4 = 0;
	bool		isNull = false;
	int		   *cc_keyno = cache->cc_keyno;
	TupleDesc	cc_tupdesc = cache->cc_tupdesc;

	/* Now extract key fields from tuple, insert into scankey */
	switch (nkeys)
	{
		case 4:
			v4 = fastgetattr(tuple,
							 cc_keyno[3],
							 cc_tupdesc,
							 &isNull);
			Assert(!isNull);
			pg_fallthrough;
		case 3:
			v3 = fastgetattr(tuple,
							 cc_keyno[2],
							 cc_tupdesc,
							 &isNull);
			Assert(!isNull);
			pg_fallthrough;
		case 2:
			v2 = fastgetattr(tuple,
							 cc_keyno[1],
							 cc_tupdesc,
							 &isNull);
			Assert(!isNull);
			pg_fallthrough;
		case 1:
			v1 = fastgetattr(tuple,
							 cc_keyno[0],
							 cc_tupdesc,
							 &isNull);
			Assert(!isNull);
			break;
		default:
			elog(FATAL, "wrong number of hash keys: %d", nkeys);
			break;
	}

	return CatalogCacheComputeHashValue(cache, nkeys, v1, v2, v3, v4);
}

/*
 *		CatalogCacheCompareTuple
 *
 * Compare a tuple to the passed arguments.
 */
static inline bool
CatalogCacheCompareTuple(const CatCache *cache, int nkeys,
						 const Datum *cachekeys,
						 const Datum *searchkeys)
{
	const CCFastEqualFN *cc_fastequal = cache->cc_fastequal;
	int			i;

	for (i = 0; i < nkeys; i++)
	{
		if (!(cc_fastequal[i]) (cachekeys[i], searchkeys[i]))
			return false;
	}
	return true;
}


#ifdef CATCACHE_STATS

static void
CatCachePrintStats(int code, Datum arg)
{
	slist_iter	iter;
	uint64		cc_searches = 0;
	uint64		cc_hits = 0;
	uint64		cc_neg_hits = 0;
	uint64		cc_newloads = 0;
	uint64		cc_invals = 0;
	uint64		cc_nlists = 0;
	uint64		cc_lsearches = 0;
	uint64		cc_lhits = 0;

	slist_foreach(iter, &CacheHdr->ch_caches)
	{
		CatCache   *cache = slist_container(CatCache, cc_next, iter.cur);

		if (cache->cc_ntup == 0 && cache->cc_searches == 0)
			continue;			/* don't print unused caches */
		elog(DEBUG2, "catcache %s/%u: %d tup, %" PRIu64 " srch, %" PRIu64 "+%"
			 PRIu64 "=%" PRIu64 " hits, %" PRIu64 "+%" PRIu64 "=%"
			 PRIu64 " loads, %" PRIu64 " invals, %d lists, %" PRIu64
			 " lsrch, %" PRIu64 " lhits",
			 cache->cc_relname,
			 cache->cc_indexoid,
			 cache->cc_ntup,
			 cache->cc_searches,
			 cache->cc_hits,
			 cache->cc_neg_hits,
			 cache->cc_hits + cache->cc_neg_hits,
			 cache->cc_newloads,
			 cache->cc_searches - cache->cc_hits - cache->cc_neg_hits - cache->cc_newloads,
			 cache->cc_searches - cache->cc_hits - cache->cc_neg_hits,
			 cache->cc_invals,
			 cache->cc_nlist,
			 cache->cc_lsearches,
			 cache->cc_lhits);
		cc_searches += cache->cc_searches;
		cc_hits += cache->cc_hits;
		cc_neg_hits += cache->cc_neg_hits;
		cc_newloads += cache->cc_newloads;
		cc_invals += cache->cc_invals;
		cc_nlists += cache->cc_nlist;
		cc_lsearches += cache->cc_lsearches;
		cc_lhits += cache->cc_lhits;
	}
	elog(DEBUG2, "catcache totals: %d tup, %" PRIu64 " srch, %" PRIu64 "+%"
		 PRIu64 "=%" PRIu64 " hits, %" PRIu64 "+%" PRIu64 "=%" PRIu64
		 " loads, %" PRIu64 " invals, %" PRIu64 " lists, %" PRIu64
		 " lsrch, %" PRIu64 " lhits",
		 CacheHdr->ch_ntup,
		 cc_searches,
		 cc_hits,
		 cc_neg_hits,
		 cc_hits + cc_neg_hits,
		 cc_newloads,
		 cc_searches - cc_hits - cc_neg_hits - cc_newloads,
		 cc_searches - cc_hits - cc_neg_hits,
		 cc_invals,
		 cc_nlists,
		 cc_lsearches,
		 cc_lhits);
}
#endif							/* CATCACHE_STATS */


/*
 *		CatCacheRemoveCTup
 *
 * Unlink and delete the given cache entry
 *
 * NB: if it is a member of a CatCList, the CatCList is deleted too.
 * Both the cache entry and the list had better have zero refcount.
 */
static void
CatCacheRemoveCTup(CatCache *cache, CatCTup *ct)
{
	Assert(ct->refcount == 0);
	Assert(ct->my_cache == cache);

	if (ct->c_list)
	{
		/*
		 * The cleanest way to handle this is to call CatCacheRemoveCList,
		 * which will recurse back to me, and the recursive call will do the
		 * work.  Set the "dead" flag to make sure it does recurse.
		 */
		ct->dead = true;
		CatCacheRemoveCList(cache, ct->c_list);
		return;					/* nothing left to do */
	}

	/* delink from linked list */
	dlist_delete(&ct->cache_elem);

	/*
	 * Free keys when we're dealing with a negative entry, normal entries just
	 * point into tuple, allocated together with the CatCTup.
	 */
	if (ct->negative)
		CatCacheFreeKeys(cache->cc_tupdesc, cache->cc_nkeys,
						 cache->cc_keyno, ct->keys);

	pfree(ct);

	--cache->cc_ntup;
	--CacheHdr->ch_ntup;
}

/*
 *		CatCacheRemoveCList
 *
 * Unlink and delete the given cache list entry
 *
 * NB: any dead member entries that become unreferenced are deleted too.
 */
static void
CatCacheRemoveCList(CatCache *cache, CatCList *cl)
{
	int			i;

	Assert(cl->refcount == 0);
	Assert(cl->my_cache == cache);

	/* delink from member tuples */
	for (i = cl->n_members; --i >= 0;)
	{
		CatCTup    *ct = cl->members[i];

		Assert(ct->c_list == cl);
		ct->c_list = NULL;
		/* if the member is dead and now has no references, remove it */
		if (
#ifndef CATCACHE_FORCE_RELEASE
			ct->dead &&
#endif
			ct->refcount == 0)
			CatCacheRemoveCTup(cache, ct);
	}

	/* delink from linked list */
	dlist_delete(&cl->cache_elem);

	/* free associated column data */
	CatCacheFreeKeys(cache->cc_tupdesc, cl->nkeys,
					 cache->cc_keyno, cl->keys);

	pfree(cl);

	--cache->cc_nlist;
}


/*
 *	CatCacheInvalidate
 *
 *	Invalidate entries in the specified cache, given a hash value.
 *
 *	We delete cache entries that match the hash value, whether positive
 *	or negative.  We don't care whether the invalidation is the result
 *	of a tuple insertion or a deletion.
 *
 *	We used to try to match positive cache entries by TID, but that is
 *	unsafe after a VACUUM FULL on a system catalog: an inval event could
 *	be queued before VACUUM FULL, and then processed afterwards, when the
 *	target tuple that has to be invalidated has a different TID than it
 *	did when the event was created.  So now we just compare hash values and
 *	accept the small risk of unnecessary invalidations due to false matches.
 *
 *	This routine is only quasi-public: it should only be used by inval.c.
 */
void
CatCacheInvalidate(CatCache *cache, uint32 hashValue)
{
	Index		hashIndex;
	dlist_mutable_iter iter;

	CACHE_elog(DEBUG2, "CatCacheInvalidate: called");

	/*
	 * We don't bother to check whether the cache has finished initialization
	 * yet; if not, there will be no entries in it so no problem.
	 */

	/*
	 * Invalidate *all* CatCLists in this cache; it's too hard to tell which
	 * searches might still be correct, so just zap 'em all.
	 */
	for (int i = 0; i < cache->cc_nlbuckets; i++)
	{
		dlist_head *bucket = &cache->cc_lbucket[i];

		dlist_foreach_modify(iter, bucket)
		{
			CatCList   *cl = dlist_container(CatCList, cache_elem, iter.cur);

			if (cl->refcount > 0)
				cl->dead = true;
			else
				CatCacheRemoveCList(cache, cl);
		}
	}

	/*
	 * inspect the proper hash bucket for tuple matches
	 */
	hashIndex = HASH_INDEX(hashValue, cache->cc_nbuckets);
	dlist_foreach_modify(iter, &cache->cc_bucket[hashIndex])
	{
		CatCTup    *ct = dlist_container(CatCTup, cache_elem, iter.cur);

		if (hashValue == ct->hash_value)
		{
			if (ct->refcount > 0 ||
				(ct->c_list && ct->c_list->refcount > 0))
			{
				ct->dead = true;
				/* list, if any, was marked dead above */
				Assert(ct->c_list == NULL || ct->c_list->dead);
			}
			else
				CatCacheRemoveCTup(cache, ct);
			CACHE_elog(DEBUG2, "CatCacheInvalidate: invalidated");
#ifdef CATCACHE_STATS
			cache->cc_invals++;
#endif
			/* could be multiple matches, so keep looking! */
		}
	}

	/* Also invalidate any entries that are being built */
	for (CatCInProgress *e = catcache_in_progress_stack; e != NULL; e = e->next)
	{
		if (e->cache == cache)
		{
			if (e->list || e->hash_value == hashValue)
				e->dead = true;
		}
	}
}

/* ----------------------------------------------------------------
 *					   public functions
 * ----------------------------------------------------------------
 */


/*
 * Standard routine for creating cache context if it doesn't exist yet
 *
 * There are a lot of places (probably far more than necessary) that check
 * whether CacheMemoryContext exists yet and want to create it if not.
 * We centralize knowledge of exactly how to create it here.
 */
void
CreateCacheMemoryContext(void)
{
	/*
	 * Purely for paranoia, check that context doesn't exist; caller probably
	 * did so already.
	 */
	if (!CacheMemoryContext)
		CacheMemoryContext = AllocSetContextCreate(TopMemoryContext,
												   "CacheMemoryContext",
												   ALLOCSET_DEFAULT_SIZES);
}


/*
 *		ResetCatalogCache
 *
 * Reset one catalog cache to empty.
 *
 * This is not very efficient if the target cache is nearly empty.
 * However, it shouldn't need to be efficient; we don't invoke it often.
 *
 * If 'debug_discard' is true, we are being called as part of
 * debug_discard_caches.  In that case, the cache is not reset for
 * correctness, but just to get more testing of cache invalidation.  We skip
 * resetting in-progress build entries in that case, or we'd never make any
 * progress.
 */
static void
ResetCatalogCache(CatCache *cache, bool debug_discard)
{
	dlist_mutable_iter iter;
	int			i;

	/* Remove each list in this cache, or at least mark it dead */
	for (i = 0; i < cache->cc_nlbuckets; i++)
	{
		dlist_head *bucket = &cache->cc_lbucket[i];

		dlist_foreach_modify(iter, bucket)
		{
			CatCList   *cl = dlist_container(CatCList, cache_elem, iter.cur);

			if (cl->refcount > 0)
				cl->dead = true;
			else
				CatCacheRemoveCList(cache, cl);
		}
	}

	/* Remove each tuple in this cache, or at least mark it dead */
	for (i = 0; i < cache->cc_nbuckets; i++)
	{
		dlist_head *bucket = &cache->cc_bucket[i];

		dlist_foreach_modify(iter, bucket)
		{
			CatCTup    *ct = dlist_container(CatCTup, cache_elem, iter.cur);

			if (ct->refcount > 0 ||
				(ct->c_list && ct->c_list->refcount > 0))
			{
				ct->dead = true;
				/* list, if any, was marked dead above */
				Assert(ct->c_list == NULL || ct->c_list->dead);
			}
			else
				CatCacheRemoveCTup(cache, ct);
#ifdef CATCACHE_STATS
			cache->cc_invals++;
#endif
		}
	}

	/* Also invalidate any entries that are being built */
	if (!debug_discard)
	{
		for (CatCInProgress *e = catcache_in_progress_stack; e != NULL; e = e->next)
		{
			if (e->cache == cache)
				e->dead = true;
		}
	}
}

/*
 *		ResetCatalogCaches
 *
 * Reset all caches when a shared cache inval event forces it
 */
void
ResetCatalogCaches(void)
{
	ResetCatalogCachesExt(false);
}

void
ResetCatalogCachesExt(bool debug_discard)
{
	slist_iter	iter;

	CACHE_elog(DEBUG2, "ResetCatalogCaches called");

	slist_foreach(iter, &CacheHdr->ch_caches)
	{
		CatCache   *cache = slist_container(CatCache, cc_next, iter.cur);

		ResetCatalogCache(cache, debug_discard);
	}

	CACHE_elog(DEBUG2, "end of ResetCatalogCaches call");
}

/*
 *		CatalogCacheFlushCatalog
 *
 *	Flush all catcache entries that came from the specified system catalog.
 *	This is needed after VACUUM FULL/CLUSTER on the catalog, since the
 *	tuples very likely now have different TIDs than before.  (At one point
 *	we also tried to force re-execution of CatalogCacheInitializeCache for
 *	the cache(s) on that catalog.  This is a bad idea since it leads to all
 *	kinds of trouble if a cache flush occurs while loading cache entries.
 *	We now avoid the need to do it by copying cc_tupdesc out of the relcache,
 *	rather than relying on the relcache to keep a tupdesc for us.  Of course
 *	this assumes the tupdesc of a cacheable system table will not change...)
 */
void
CatalogCacheFlushCatalog(Oid catId)
{
	slist_iter	iter;

	CACHE_elog(DEBUG2, "CatalogCacheFlushCatalog called for %u", catId);

	slist_foreach(iter, &CacheHdr->ch_caches)
	{
		CatCache   *cache = slist_container(CatCache, cc_next, iter.cur);

		/* Does this cache store tuples of the target catalog? */
		if (cache->cc_reloid == catId)
		{
			/* Yes, so flush all its contents */
			ResetCatalogCache(cache, false);

			/* Tell inval.c to call syscache callbacks for this cache */
			CallSyscacheCallbacks(cache->id, 0);
		}
	}

	CACHE_elog(DEBUG2, "end of CatalogCacheFlushCatalog call");
}

/*
 *		InitCatCache
 *
 *	This allocates and initializes a cache for a system catalog relation.
 *	Actually, the cache is only partially initialized to avoid opening the
 *	relation.  The relation will be opened and the rest of the cache
 *	structure initialized on the first access.
 */
#ifdef CACHEDEBUG
#define InitCatCache_DEBUG2 \
do { \
	elog(DEBUG2, "InitCatCache: rel=%u ind=%u id=%d nkeys=%d size=%d", \
		 cp->cc_reloid, cp->cc_indexoid, cp->id, \
		 cp->cc_nkeys, cp->cc_nbuckets); \
} while(0)
#else
#define InitCatCache_DEBUG2
#endif

CatCache *
InitCatCache(int id,
			 Oid reloid,
			 Oid indexoid,
			 int nkeys,
			 const int *key,
			 int nbuckets)
{
	CatCache   *cp;
	MemoryContext oldcxt;
	int			i;

	/*
	 * nbuckets is the initial number of hash buckets to use in this catcache.
	 * It will be enlarged later if it becomes too full.
	 *
	 * nbuckets must be a power of two.  We check this via Assert rather than
	 * a full runtime check because the values will be coming from constant
	 * tables.
	 *
	 * If you're confused by the power-of-two check, see comments in
	 * bitmapset.c for an explanation.
	 */
	Assert(nbuckets > 0 && (nbuckets & -nbuckets) == nbuckets);

	/*
	 * first switch to the cache context so our allocations do not vanish at
	 * the end of a transaction
	 */
	if (!CacheMemoryContext)
		CreateCacheMemoryContext();

	oldcxt = MemoryContextSwitchTo(CacheMemoryContext);

	/*
	 * if first time through, initialize the cache group header
	 */
	if (CacheHdr == NULL)
	{
		CacheHdr = palloc_object(CatCacheHeader);
		slist_init(&CacheHdr->ch_caches);
		CacheHdr->ch_ntup = 0;
#ifdef CATCACHE_STATS
		/* set up to dump stats at backend exit */
		on_proc_exit(CatCachePrintStats, 0);
#endif
	}

	/*
	 * Allocate a new cache structure, aligning to a cacheline boundary
	 *
	 * Note: we rely on zeroing to initialize all the dlist headers correctly
	 */
	cp = (CatCache *) palloc_aligned(sizeof(CatCache), PG_CACHE_LINE_SIZE,
									 MCXT_ALLOC_ZERO);
	cp->cc_bucket = palloc0(nbuckets * sizeof(dlist_head));

	/*
	 * Many catcaches never receive any list searches.  Therefore, we don't
	 * allocate the cc_lbuckets till we get a list search.
	 */
	cp->cc_lbucket = NULL;

	/*
	 * initialize the cache's relation information for the relation
	 * corresponding to this cache, and initialize some of the new cache's
	 * other internal fields.  But don't open the relation yet.
	 */
	cp->id = id;
	cp->cc_relname = "(not known yet)";
	cp->cc_reloid = reloid;
	cp->cc_indexoid = indexoid;
	cp->cc_relisshared = false; /* temporary */
	cp->cc_tupdesc = (TupleDesc) NULL;
	cp->cc_ntup = 0;
	cp->cc_nlist = 0;
	cp->cc_nbuckets = nbuckets;
	cp->cc_nlbuckets = 0;
	cp->cc_nkeys = nkeys;
	for (i = 0; i < nkeys; ++i)
	{
		Assert(AttributeNumberIsValid(key[i]));
		cp->cc_keyno[i] = key[i];
	}

	/*
	 * new cache is initialized as far as we can go for now. print some
	 * debugging information, if appropriate.
	 */
	InitCatCache_DEBUG2;

	/*
	 * add completed cache to top of group header's list
	 */
	slist_push_head(&CacheHdr->ch_caches, &cp->cc_next);

	/*
	 * back to the old context before we return...
	 */
	MemoryContextSwitchTo(oldcxt);

	return cp;
}

/*
 * Enlarge a catcache, doubling the number of buckets.
 */
static void
RehashCatCache(CatCache *cp)
{
	dlist_head *newbucket;
	int			newnbuckets;
	int			i;

	elog(DEBUG1, "rehashing catalog cache id %d for %s; %d tups, %d buckets",
		 cp->id, cp->cc_relname, cp->cc_ntup, cp->cc_nbuckets);

	/* Allocate a new, larger, hash table. */
	newnbuckets = cp->cc_nbuckets * 2;
	newbucket = (dlist_head *) MemoryContextAllocZero(CacheMemoryContext, newnbuckets * sizeof(dlist_head));

	/* Move all entries from old hash table to new. */
	for (i = 0; i < cp->cc_nbuckets; i++)
	{
		dlist_mutable_iter iter;

		dlist_foreach_modify(iter, &cp->cc_bucket[i])
		{
			CatCTup    *ct = dlist_container(CatCTup, cache_elem, iter.cur);
			int			hashIndex = HASH_INDEX(ct->hash_value, newnbuckets);

			dlist_delete(iter.cur);

			/*
			 * Note that each item is pushed at the tail of the new bucket,
			 * not its head.  This is consistent with the SearchCatCache*()
			 * routines, where matching entries are moved at the front of the
			 * list to speed subsequent searches.
			 */
			dlist_push_tail(&newbucket[hashIndex], &ct->cache_elem);
		}
	}

	/* Switch to the new array. */
	pfree(cp->cc_bucket);
	cp->cc_nbuckets = newnbuckets;
	cp->cc_bucket = newbucket;
}

/*
 * Enlarge a catcache's list storage, doubling the number of buckets.
 */
static void
RehashCatCacheLists(CatCache *cp)
{
	dlist_head *newbucket;
	int			newnbuckets;
	int			i;

	elog(DEBUG1, "rehashing catalog cache id %d for %s; %d lists, %d buckets",
		 cp->id, cp->cc_relname, cp->cc_nlist, cp->cc_nlbuckets);

	/* Allocate a new, larger, hash table. */
	newnbuckets = cp->cc_nlbuckets * 2;
	newbucket = (dlist_head *) MemoryContextAllocZero(CacheMemoryContext, newnbuckets * sizeof(dlist_head));

	/* Move all entries from old hash table to new. */
	for (i = 0; i < cp->cc_nlbuckets; i++)
	{
		dlist_mutable_iter iter;

		dlist_foreach_modify(iter, &cp->cc_lbucket[i])
		{
			CatCList   *cl = dlist_container(CatCList, cache_elem, iter.cur);
			int			hashIndex = HASH_INDEX(cl->hash_value, newnbuckets);

			dlist_delete(iter.cur);

			/*
			 * Note that each item is pushed at the tail of the new bucket,
			 * not its head.  This is consistent with the SearchCatCache*()
			 * routines, where matching entries are moved at the front of the
			 * list to speed subsequent searches.
			 */
			dlist_push_tail(&newbucket[hashIndex], &cl->cache_elem);
		}
	}

	/* Switch to the new array. */
	pfree(cp->cc_lbucket);
	cp->cc_nlbuckets = newnbuckets;
	cp->cc_lbucket = newbucket;
}

/*
 *		ConditionalCatalogCacheInitializeCache
 *
 * Call CatalogCacheInitializeCache() if not yet done.
 */
pg_attribute_always_inline
static void
ConditionalCatalogCacheInitializeCache(CatCache *cache)
{
#ifdef USE_ASSERT_CHECKING
	/*
	 * TypeCacheRelCallback() runs outside transactions and relies on TYPEOID
	 * for hashing.  This isn't ideal.  Since lookup_type_cache() both
	 * registers the callback and searches TYPEOID, reaching trouble likely
	 * requires OOM at an unlucky moment.
	 *
	 * InvalidateAttoptCacheCallback() runs outside transactions and likewise
	 * relies on ATTNUM.  InitPostgres() initializes ATTNUM, so it's reliable.
	 */
	if (!(cache->id == TYPEOID || cache->id == ATTNUM) ||
		IsTransactionState())
		AssertCouldGetRelation();
	else
		Assert(cache->cc_tupdesc != NULL);
#endif

	if (unlikely(cache->cc_tupdesc == NULL))
		CatalogCacheInitializeCache(cache);
}

/*
 *		CatalogCacheInitializeCache
 *
 * This function does final initialization of a catcache: obtain the tuple
 * descriptor and set up the hash and equality function links.
 */
#ifdef CACHEDEBUG
#define CatalogCacheInitializeCache_DEBUG1 \
	elog(DEBUG2, "CatalogCacheInitializeCache: cache @%p rel=%u", cache, \
		 cache->cc_reloid)

#define CatalogCacheInitializeCache_DEBUG2 \
do { \
		if (cache->cc_keyno[i] > 0) { \
			elog(DEBUG2, "CatalogCacheInitializeCache: load %d/%d w/%d, %u", \
				i+1, cache->cc_nkeys, cache->cc_keyno[i], \
				 TupleDescAttr(tupdesc, cache->cc_keyno[i] - 1)->atttypid); \
		} else { \
			elog(DEBUG2, "CatalogCacheInitializeCache: load %d/%d w/%d", \
				i+1, cache->cc_nkeys, cache->cc_keyno[i]); \
		} \
} while(0)
#else
#define CatalogCacheInitializeCache_DEBUG1
#define CatalogCacheInitializeCache_DEBUG2
#endif

static void
CatalogCacheInitializeCache(CatCache *cache)
{
	Relation	relation;
	MemoryContext oldcxt;
	TupleDesc	tupdesc;
	int			i;

	CatalogCacheInitializeCache_DEBUG1;

#ifdef USE_FASTPG
	if (FastPgCatalogCacheInitializeCache(cache))
		return;
	if (!IsUnderPostmaster)
	{
		uint8_t		policy =
			fastpg_rust_catalog_policy_by_relation_oid((uint32_t) cache->cc_reloid);

		if (policy != 0)
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("fastpg virtual catalog %u is declared but has no tuple descriptor",
							cache->cc_reloid)));
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("fastpg virtual catalog does not support relation %u",
						cache->cc_reloid)));
	}
#endif

	relation = table_open(cache->cc_reloid, AccessShareLock);

	/*
	 * switch to the cache context so our allocations do not vanish at the end
	 * of a transaction
	 */
	Assert(CacheMemoryContext != NULL);

	oldcxt = MemoryContextSwitchTo(CacheMemoryContext);

	/*
	 * copy the relcache's tuple descriptor to permanent cache storage
	 */
	tupdesc = CreateTupleDescCopyConstr(RelationGetDescr(relation));

	/*
	 * save the relation's name and relisshared flag, too (cc_relname is used
	 * only for debugging purposes)
	 */
	cache->cc_relname = pstrdup(RelationGetRelationName(relation));
	cache->cc_relisshared = RelationGetForm(relation)->relisshared;

	/*
	 * return to the caller's memory context and close the rel
	 */
	MemoryContextSwitchTo(oldcxt);

	table_close(relation, AccessShareLock);

	CACHE_elog(DEBUG2, "CatalogCacheInitializeCache: %s, %d keys",
			   cache->cc_relname, cache->cc_nkeys);

	/*
	 * initialize cache's key information
	 */
	for (i = 0; i < cache->cc_nkeys; ++i)
	{
		Oid			keytype;
		RegProcedure eqfunc;

		CatalogCacheInitializeCache_DEBUG2;

		if (cache->cc_keyno[i] > 0)
		{
			Form_pg_attribute attr = TupleDescAttr(tupdesc,
												   cache->cc_keyno[i] - 1);

			keytype = attr->atttypid;
			/* cache key columns should always be NOT NULL */
			Assert(attr->attnotnull);
		}
		else
		{
			if (cache->cc_keyno[i] < 0)
				elog(FATAL, "sys attributes are not supported in caches");
			keytype = OIDOID;
		}

		GetCCHashEqFuncs(keytype,
						 &cache->cc_hashfunc[i],
						 &eqfunc,
						 &cache->cc_fastequal[i]);

		/*
		 * Do equality-function lookup (we assume this won't need a catalog
		 * lookup for any supported type)
		 */
		fmgr_info_cxt(eqfunc,
					  &cache->cc_skey[i].sk_func,
					  CacheMemoryContext);

		/* Initialize sk_attno suitably for HeapKeyTest() and heap scans */
		cache->cc_skey[i].sk_attno = cache->cc_keyno[i];

		/* Fill in sk_strategy as well --- always standard equality */
		cache->cc_skey[i].sk_strategy = BTEqualStrategyNumber;
		cache->cc_skey[i].sk_subtype = InvalidOid;
		/* If a catcache key requires a collation, it must be C collation */
		cache->cc_skey[i].sk_collation = C_COLLATION_OID;

		CACHE_elog(DEBUG2, "CatalogCacheInitializeCache %s %d %p",
				   cache->cc_relname, i, cache);
	}

	/*
	 * mark this cache fully initialized
	 */
	cache->cc_tupdesc = tupdesc;
}

/*
 * InitCatCachePhase2 -- external interface for CatalogCacheInitializeCache
 *
 * One reason to call this routine is to ensure that the relcache has
 * created entries for all the catalogs and indexes referenced by catcaches.
 * Therefore, provide an option to open the index as well as fixing the
 * cache itself.  An exception is the indexes on pg_am, which we don't use
 * (cf. IndexScanOK).
 */
void
InitCatCachePhase2(CatCache *cache, bool touch_index)
{
	ConditionalCatalogCacheInitializeCache(cache);

	if (touch_index &&
		cache->id != AMOID &&
		cache->id != AMNAME)
	{
		Relation	idesc;

		/*
		 * We must lock the underlying catalog before opening the index to
		 * avoid deadlock, since index_open could possibly result in reading
		 * this same catalog, and if anyone else is exclusive-locking this
		 * catalog and index they'll be doing it in that order.
		 */
		LockRelationOid(cache->cc_reloid, AccessShareLock);
		idesc = index_open(cache->cc_indexoid, AccessShareLock);

		/*
		 * While we've got the index open, let's check that it's unique (and
		 * not just deferrable-unique, thank you very much).  This is just to
		 * catch thinkos in definitions of new catcaches, so we don't worry
		 * about the pg_am indexes not getting tested.
		 */
		Assert(idesc->rd_index->indisunique &&
			   idesc->rd_index->indimmediate);

		index_close(idesc, AccessShareLock);
		UnlockRelationOid(cache->cc_reloid, AccessShareLock);
	}
}


/*
 *		IndexScanOK
 *
 *		This function checks for tuples that will be fetched by
 *		IndexSupportInitialize() during relcache initialization for
 *		certain system indexes that support critical syscaches.
 *		We can't use an indexscan to fetch these, else we'll get into
 *		infinite recursion.  A plain heap scan will work, however.
 *		Once we have completed relcache initialization (signaled by
 *		criticalRelcachesBuilt), we don't have to worry anymore.
 *
 *		Similarly, during backend startup we have to be able to use the
 *		pg_authid, pg_auth_members and pg_database syscaches for
 *		authentication even if we don't yet have relcache entries for those
 *		catalogs' indexes.
 */
static bool
IndexScanOK(CatCache *cache)
{
	switch (cache->id)
	{
		case INDEXRELID:

			/*
			 * Rather than tracking exactly which indexes have to be loaded
			 * before we can use indexscans (which changes from time to time),
			 * just force all pg_index searches to be heap scans until we've
			 * built the critical relcaches.
			 */
			if (!criticalRelcachesBuilt)
				return false;
			break;

		case AMOID:
		case AMNAME:

			/*
			 * Always do heap scans in pg_am, because it's so small there's
			 * not much point in an indexscan anyway.  We *must* do this when
			 * initially building critical relcache entries, but we might as
			 * well just always do it.
			 */
			return false;

		case AUTHNAME:
		case AUTHOID:
		case AUTHMEMMEMROLE:
		case DATABASEOID:

			/*
			 * Protect authentication lookups occurring before relcache has
			 * collected entries for shared indexes.
			 */
			if (!criticalSharedRelcachesBuilt)
				return false;
			break;

		default:
			break;
	}

	/* Normal case, allow index scan */
	return true;
}

/*
 *	SearchCatCache
 *
 *		This call searches a system cache for a tuple, opening the relation
 *		if necessary (on the first access to a particular cache).
 *
 *		The result is NULL if not found, or a pointer to a HeapTuple in
 *		the cache.  The caller must not modify the tuple, and must call
 *		ReleaseCatCache() when done with it.
 *
 * The search key values should be expressed as Datums of the key columns'
 * datatype(s).  (Pass zeroes for any unused parameters.)  As a special
 * exception, the passed-in key for a NAME column can be just a C string;
 * the caller need not go to the trouble of converting it to a fully
 * null-padded NAME.
 */
HeapTuple
SearchCatCache(CatCache *cache,
			   Datum v1,
			   Datum v2,
			   Datum v3,
			   Datum v4)
{
	return SearchCatCacheInternal(cache, cache->cc_nkeys, v1, v2, v3, v4);
}


/*
 * SearchCatCacheN() are SearchCatCache() versions for a specific number of
 * arguments. The compiler can inline the body and unroll loops, making them a
 * bit faster than SearchCatCache().
 */

HeapTuple
SearchCatCache1(CatCache *cache,
				Datum v1)
{
	return SearchCatCacheInternal(cache, 1, v1, 0, 0, 0);
}


HeapTuple
SearchCatCache2(CatCache *cache,
				Datum v1, Datum v2)
{
	return SearchCatCacheInternal(cache, 2, v1, v2, 0, 0);
}


HeapTuple
SearchCatCache3(CatCache *cache,
				Datum v1, Datum v2, Datum v3)
{
	return SearchCatCacheInternal(cache, 3, v1, v2, v3, 0);
}


HeapTuple
SearchCatCache4(CatCache *cache,
				Datum v1, Datum v2, Datum v3, Datum v4)
{
	return SearchCatCacheInternal(cache, 4, v1, v2, v3, v4);
}

/*
 * Work-horse for SearchCatCache/SearchCatCacheN.
 */
static inline HeapTuple
SearchCatCacheInternal(CatCache *cache,
					   int nkeys,
					   Datum v1,
					   Datum v2,
					   Datum v3,
					   Datum v4)
{
	Datum		arguments[CATCACHE_MAXKEYS];
	uint32		hashValue;
	Index		hashIndex;
	dlist_iter	iter;
	dlist_head *bucket;
	CatCTup    *ct;
#ifdef USE_FASTPG
	HeapTuple	ntp;
#endif

	Assert(cache->cc_nkeys == nkeys);

	/*
	 * one-time startup overhead for each cache
	 */
	ConditionalCatalogCacheInitializeCache(cache);

#ifdef CATCACHE_STATS
	cache->cc_searches++;
#endif

	/* Initialize local parameter array */
	arguments[0] = v1;
	arguments[1] = v2;
	arguments[2] = v3;
	arguments[3] = v4;

	/*
	 * find the hash bucket in which to look for the tuple
	 */
	hashValue = CatalogCacheComputeHashValue(cache, nkeys, v1, v2, v3, v4);
	hashIndex = HASH_INDEX(hashValue, cache->cc_nbuckets);

	/*
	 * scan the hash bucket until we find a match or exhaust our tuples
	 *
	 * Note: it's okay to use dlist_foreach here, even though we modify the
	 * dlist within the loop, because we don't continue the loop afterwards.
	 */
	bucket = &cache->cc_bucket[hashIndex];
	dlist_foreach(iter, bucket)
	{
		ct = dlist_container(CatCTup, cache_elem, iter.cur);

		if (ct->dead)
			continue;			/* ignore dead entries */

		if (ct->hash_value != hashValue)
			continue;			/* quickly skip entry if wrong hash val */

		if (!CatalogCacheCompareTuple(cache, nkeys, ct->keys, arguments))
			continue;

		/*
		 * We found a match in the cache.  Move it to the front of the list
		 * for its hashbucket, in order to speed subsequent searches.  (The
		 * most frequently accessed elements in any hashbucket will tend to be
		 * near the front of the hashbucket's list.)
		 */
		dlist_move_head(bucket, &ct->cache_elem);

		/*
		 * If it's a positive entry, bump its refcount and return it. If it's
		 * negative, we can report failure to the caller.
		 */
		if (!ct->negative)
		{
			ResourceOwnerEnlarge(CurrentResourceOwner);
			ct->refcount++;
			ResourceOwnerRememberCatCacheRef(CurrentResourceOwner, &ct->tuple);

			CACHE_elog(DEBUG2, "SearchCatCache(%s): found in bucket %d",
					   cache->cc_relname, hashIndex);

#ifdef CATCACHE_STATS
			cache->cc_hits++;
#endif

			return &ct->tuple;
		}
		else
		{
			CACHE_elog(DEBUG2, "SearchCatCache(%s): found neg entry in bucket %d",
					   cache->cc_relname, hashIndex);

#ifdef CATCACHE_STATS
			cache->cc_neg_hits++;
#endif

			return NULL;
		}
	}

#ifdef USE_FASTPG
	ntp = FastPgCatalogCacheLookup(cache, nkeys, arguments);
	if (HeapTupleIsValid(ntp))
	{
		ct = CatalogCacheCreateEntry(cache, ntp, arguments, hashValue, hashIndex);
		heap_freetuple(ntp);
		if (ct == NULL)
			return NULL;

		ResourceOwnerEnlarge(CurrentResourceOwner);
		ct->refcount++;
		ResourceOwnerRememberCatCacheRef(CurrentResourceOwner, &ct->tuple);

		return &ct->tuple;
	}
	if (FastPgCatalogCacheHandlesMiss(cache, nkeys))
	{
		ct = CatalogCacheCreateEntry(cache, NULL, arguments, hashValue, hashIndex);
		Assert(ct != NULL);
		return NULL;
	}
#endif

	return SearchCatCacheMiss(cache, nkeys, hashValue, hashIndex, v1, v2, v3, v4);
}

/*
 * Search the actual catalogs, rather than the cache.
 *
 * This is kept separate from SearchCatCacheInternal() to keep the fast-path
 * as small as possible.  To avoid that effort being undone by a helpful
 * compiler, try to explicitly forbid inlining.
 */
static pg_noinline HeapTuple
SearchCatCacheMiss(CatCache *cache,
				   int nkeys,
				   uint32 hashValue,
				   Index hashIndex,
				   Datum v1,
				   Datum v2,
				   Datum v3,
				   Datum v4)
{
	ScanKeyData cur_skey[CATCACHE_MAXKEYS];
	Relation	relation;
	SysScanDesc scandesc;
	HeapTuple	ntp;
	CatCTup    *ct;
	bool		stale;
	Datum		arguments[CATCACHE_MAXKEYS];

	/* Initialize local parameter array */
	arguments[0] = v1;
	arguments[1] = v2;
	arguments[2] = v3;
	arguments[3] = v4;

	/*
	 * Tuple was not found in cache, so we have to try to retrieve it directly
	 * from the relation.  If found, we will add it to the cache; if not
	 * found, we will add a negative cache entry instead.
	 *
	 * NOTE: it is possible for recursive cache lookups to occur while reading
	 * the relation --- for example, due to shared-cache-inval messages being
	 * processed during table_open().  This is OK.  It's even possible for one
	 * of those lookups to find and enter the very same tuple we are trying to
	 * fetch here.  If that happens, we will enter a second copy of the tuple
	 * into the cache.  The first copy will never be referenced again, and
	 * will eventually age out of the cache, so there's no functional problem.
	 * This case is rare enough that it's not worth expending extra cycles to
	 * detect.
	 *
	 * Another case, which we *must* handle, is that the tuple could become
	 * outdated during CatalogCacheCreateEntry's attempt to detoast it (since
	 * AcceptInvalidationMessages can run during TOAST table access).  We do
	 * not want to return already-stale catcache entries, so we loop around
	 * and do the table scan again if that happens.
	 */
	relation = table_open(cache->cc_reloid, AccessShareLock);

	/*
	 * Ok, need to make a lookup in the relation, copy the scankey and fill
	 * out any per-call fields.
	 */
	memcpy(cur_skey, cache->cc_skey, sizeof(ScanKeyData) * nkeys);
	cur_skey[0].sk_argument = v1;
	cur_skey[1].sk_argument = v2;
	cur_skey[2].sk_argument = v3;
	cur_skey[3].sk_argument = v4;

	do
	{
		scandesc = systable_beginscan(relation,
									  cache->cc_indexoid,
									  IndexScanOK(cache),
									  NULL,
									  nkeys,
									  cur_skey);

		ct = NULL;
		stale = false;

		while (HeapTupleIsValid(ntp = systable_getnext(scandesc)))
		{
			ct = CatalogCacheCreateEntry(cache, ntp, NULL,
										 hashValue, hashIndex);
			/* upon failure, we must start the scan over */
			if (ct == NULL)
			{
				stale = true;
				break;
			}
			/* immediately set the refcount to 1 */
			ResourceOwnerEnlarge(CurrentResourceOwner);
			ct->refcount++;
			ResourceOwnerRememberCatCacheRef(CurrentResourceOwner, &ct->tuple);
			break;				/* assume only one match */
		}

		systable_endscan(scandesc);
	} while (stale);

	table_close(relation, AccessShareLock);

	/*
	 * If tuple was not found, we need to build a negative cache entry
	 * containing a fake tuple.  The fake tuple has the correct key columns,
	 * but nulls everywhere else.
	 *
	 * In bootstrap mode, we don't build negative entries, because the cache
	 * invalidation mechanism isn't alive and can't clear them if the tuple
	 * gets created later.  (Bootstrap doesn't do UPDATEs, so it doesn't need
	 * cache inval for that.)
	 */
	if (ct == NULL)
	{
		if (IsBootstrapProcessingMode())
			return NULL;

		ct = CatalogCacheCreateEntry(cache, NULL, arguments,
									 hashValue, hashIndex);

		/* Creating a negative cache entry shouldn't fail */
		Assert(ct != NULL);

		CACHE_elog(DEBUG2, "SearchCatCache(%s): Contains %d/%d tuples",
				   cache->cc_relname, cache->cc_ntup, CacheHdr->ch_ntup);
		CACHE_elog(DEBUG2, "SearchCatCache(%s): put neg entry in bucket %d",
				   cache->cc_relname, hashIndex);

		/*
		 * We are not returning the negative entry to the caller, so leave its
		 * refcount zero.
		 */

		return NULL;
	}

	CACHE_elog(DEBUG2, "SearchCatCache(%s): Contains %d/%d tuples",
			   cache->cc_relname, cache->cc_ntup, CacheHdr->ch_ntup);
	CACHE_elog(DEBUG2, "SearchCatCache(%s): put in bucket %d",
			   cache->cc_relname, hashIndex);

#ifdef CATCACHE_STATS
	cache->cc_newloads++;
#endif

	return &ct->tuple;
}

/*
 *	ReleaseCatCache
 *
 *	Decrement the reference count of a catcache entry (releasing the
 *	hold grabbed by a successful SearchCatCache).
 *
 *	NOTE: if compiled with -DCATCACHE_FORCE_RELEASE then catcache entries
 *	will be freed as soon as their refcount goes to zero.  In combination
 *	with aset.c's CLOBBER_FREED_MEMORY option, this provides a good test
 *	to catch references to already-released catcache entries.
 */
void
ReleaseCatCache(HeapTuple tuple)
{
	ReleaseCatCacheWithOwner(tuple, CurrentResourceOwner);
}

static void
ReleaseCatCacheWithOwner(HeapTuple tuple, ResourceOwner resowner)
{
	CatCTup    *ct = (CatCTup *) (((char *) tuple) -
								  offsetof(CatCTup, tuple));

	/* Safety checks to ensure we were handed a cache entry */
	Assert(ct->ct_magic == CT_MAGIC);
	Assert(ct->refcount > 0);

	ct->refcount--;
	if (resowner)
		ResourceOwnerForgetCatCacheRef(resowner, &ct->tuple);

	if (
#ifndef CATCACHE_FORCE_RELEASE
		ct->dead &&
#endif
		ct->refcount == 0 &&
		(ct->c_list == NULL || ct->c_list->refcount == 0))
		CatCacheRemoveCTup(ct->my_cache, ct);
}


/*
 *	GetCatCacheHashValue
 *
 *		Compute the hash value for a given set of search keys.
 *
 * The reason for exposing this as part of the API is that the hash value is
 * exposed in cache invalidation operations, so there are places outside the
 * catcache code that need to be able to compute the hash values.
 */
uint32
GetCatCacheHashValue(CatCache *cache,
					 Datum v1,
					 Datum v2,
					 Datum v3,
					 Datum v4)
{
	/*
	 * one-time startup overhead for each cache
	 */
	ConditionalCatalogCacheInitializeCache(cache);

	/*
	 * calculate the hash value
	 */
	return CatalogCacheComputeHashValue(cache, cache->cc_nkeys, v1, v2, v3, v4);
}


/*
 *	SearchCatCacheList
 *
 *		Generate a list of all tuples matching a partial key (that is,
 *		a key specifying just the first K of the cache's N key columns).
 *
 *		It doesn't make any sense to specify all of the cache's key columns
 *		here: since the key is unique, there could be at most one match, so
 *		you ought to use SearchCatCache() instead.  Hence this function takes
 *		one fewer Datum argument than SearchCatCache() does.
 *
 *		The caller must not modify the list object or the pointed-to tuples,
 *		and must call ReleaseCatCacheList() when done with the list.
 */
CatCList *
SearchCatCacheList(CatCache *cache,
				   int nkeys,
				   Datum v1,
				   Datum v2,
				   Datum v3)
{
	Datum		v4 = 0;			/* dummy last-column value */
	Datum		arguments[CATCACHE_MAXKEYS];
	uint32		lHashValue;
	Index		lHashIndex;
	dlist_iter	iter;
	dlist_head *lbucket;
	CatCList   *cl;
	CatCTup    *ct;
	List	   *volatile ctlist;
	ListCell   *ctlist_item;
	int			nmembers;
	bool		ordered;
	HeapTuple	ntp;
	MemoryContext oldcxt;
	int			i;
	CatCInProgress *save_in_progress;
	CatCInProgress in_progress_ent;

	/*
	 * one-time startup overhead for each cache
	 */
	ConditionalCatalogCacheInitializeCache(cache);

	Assert(nkeys > 0 && nkeys < cache->cc_nkeys);

#ifdef CATCACHE_STATS
	cache->cc_lsearches++;
#endif

	/* Initialize local parameter array */
	arguments[0] = v1;
	arguments[1] = v2;
	arguments[2] = v3;
	arguments[3] = v4;

	/*
	 * If we haven't previously done a list search in this cache, create the
	 * bucket header array; otherwise, consider whether it's time to enlarge
	 * it.
	 */
	if (cache->cc_lbucket == NULL)
	{
		/* Arbitrary initial size --- must be a power of 2 */
		int			nbuckets = 16;

		cache->cc_lbucket = (dlist_head *)
			MemoryContextAllocZero(CacheMemoryContext,
								   nbuckets * sizeof(dlist_head));
		/* Don't set cc_nlbuckets if we get OOM allocating cc_lbucket */
		cache->cc_nlbuckets = nbuckets;
	}
	else
	{
		/*
		 * If the hash table has become too full, enlarge the buckets array.
		 * Quite arbitrarily, we enlarge when fill factor > 2.
		 */
		if (cache->cc_nlist > cache->cc_nlbuckets * 2)
			RehashCatCacheLists(cache);
	}

	/*
	 * Find the hash bucket in which to look for the CatCList.
	 */
	lHashValue = CatalogCacheComputeHashValue(cache, nkeys, v1, v2, v3, v4);
	lHashIndex = HASH_INDEX(lHashValue, cache->cc_nlbuckets);

	/*
	 * scan the items until we find a match or exhaust our list
	 *
	 * Note: it's okay to use dlist_foreach here, even though we modify the
	 * dlist within the loop, because we don't continue the loop afterwards.
	 */
	lbucket = &cache->cc_lbucket[lHashIndex];
	dlist_foreach(iter, lbucket)
	{
		cl = dlist_container(CatCList, cache_elem, iter.cur);

		if (cl->dead)
			continue;			/* ignore dead entries */

		if (cl->hash_value != lHashValue)
			continue;			/* quickly skip entry if wrong hash val */

		/*
		 * see if the cached list matches our key.
		 */
		if (cl->nkeys != nkeys)
			continue;

		if (!CatalogCacheCompareTuple(cache, nkeys, cl->keys, arguments))
			continue;

		/*
		 * We found a matching list.  Move the list to the front of the list
		 * for its hashbucket, so as to speed subsequent searches.  (We do not
		 * move the members to the fronts of their hashbucket lists, however,
		 * since there's no point in that unless they are searched for
		 * individually.)
		 */
		dlist_move_head(lbucket, &cl->cache_elem);

		/* Bump the list's refcount and return it */
		ResourceOwnerEnlarge(CurrentResourceOwner);
		cl->refcount++;
		ResourceOwnerRememberCatCacheListRef(CurrentResourceOwner, cl);

		CACHE_elog(DEBUG2, "SearchCatCacheList(%s): found list",
				   cache->cc_relname);

#ifdef CATCACHE_STATS
		cache->cc_lhits++;
#endif

		return cl;
	}

#ifdef USE_FASTPG
	cl = FastPgCatalogCacheBuildList(cache, nkeys, arguments,
									 lHashValue, lbucket);
	if (cl != NULL)
		return cl;
#endif

	/*
	 * List was not found in cache, so we have to build it by reading the
	 * relation.  For each matching tuple found in the relation, use an
	 * existing cache entry if possible, else build a new one.
	 *
	 * We have to bump the member refcounts temporarily to ensure they won't
	 * get dropped from the cache while loading other members. We use a PG_TRY
	 * block to ensure we can undo those refcounts if we get an error before
	 * we finish constructing the CatCList.  ctlist must be valid throughout
	 * the PG_TRY block.
	 */
	ctlist = NIL;

	/*
	 * Cache invalidation can happen while we're building the list.
	 * CatalogCacheCreateEntry() handles concurrent invalidation of individual
	 * tuples, but it's also possible that a new entry is concurrently added
	 * that should be part of the list we're building.  Register an
	 * "in-progress" entry that will receive the invalidation, until we have
	 * built the final list entry.
	 */
	save_in_progress = catcache_in_progress_stack;
	in_progress_ent.next = catcache_in_progress_stack;
	in_progress_ent.cache = cache;
	in_progress_ent.hash_value = lHashValue;
	in_progress_ent.list = true;
	in_progress_ent.dead = false;
	catcache_in_progress_stack = &in_progress_ent;

	PG_TRY();
	{
		ScanKeyData cur_skey[CATCACHE_MAXKEYS];
		Relation	relation;
		SysScanDesc scandesc;
		bool		first_iter = true;

		relation = table_open(cache->cc_reloid, AccessShareLock);

		/*
		 * Ok, need to make a lookup in the relation, copy the scankey and
		 * fill out any per-call fields.
		 */
		memcpy(cur_skey, cache->cc_skey, sizeof(ScanKeyData) * cache->cc_nkeys);
		cur_skey[0].sk_argument = v1;
		cur_skey[1].sk_argument = v2;
		cur_skey[2].sk_argument = v3;
		cur_skey[3].sk_argument = v4;

		/*
		 * Scan the table for matching entries.  If an invalidation arrives
		 * mid-build, we will loop back here to retry.
		 */
		do
		{
			/*
			 * If we are retrying, release refcounts on any items created on
			 * the previous iteration.  We dare not try to free them if
			 * they're now unreferenced, since an error while doing that would
			 * result in the PG_CATCH below doing extra refcount decrements.
			 * Besides, we'll likely re-adopt those items in the next
			 * iteration, so it's not worth complicating matters to try to get
			 * rid of them.
			 */
			foreach(ctlist_item, ctlist)
			{
				ct = (CatCTup *) lfirst(ctlist_item);
				Assert(ct->c_list == NULL);
				Assert(ct->refcount > 0);
				ct->refcount--;
			}
			/* Reset ctlist in preparation for new try */
			ctlist = NIL;
			in_progress_ent.dead = false;

			scandesc = systable_beginscan(relation,
										  cache->cc_indexoid,
										  IndexScanOK(cache),
										  NULL,
										  nkeys,
										  cur_skey);

			/* The list will be ordered iff we are doing an index scan */
			ordered = (scandesc->irel != NULL);

			/* Injection point to help testing the recursive invalidation case */
			if (first_iter)
			{
				INJECTION_POINT("catcache-list-miss-systable-scan-started", NULL);
				first_iter = false;
			}

			while (HeapTupleIsValid(ntp = systable_getnext(scandesc)) &&
				   !in_progress_ent.dead)
			{
				uint32		hashValue;
				Index		hashIndex;
				bool		found = false;
				dlist_head *bucket;

				/*
				 * See if there's an entry for this tuple already.
				 */
				ct = NULL;
				hashValue = CatalogCacheComputeTupleHashValue(cache, cache->cc_nkeys, ntp);
				hashIndex = HASH_INDEX(hashValue, cache->cc_nbuckets);

				bucket = &cache->cc_bucket[hashIndex];
				dlist_foreach(iter, bucket)
				{
					ct = dlist_container(CatCTup, cache_elem, iter.cur);

					if (ct->dead || ct->negative)
						continue;	/* ignore dead and negative entries */

					if (ct->hash_value != hashValue)
						continue;	/* quickly skip entry if wrong hash val */

					if (!ItemPointerEquals(&(ct->tuple.t_self), &(ntp->t_self)))
						continue;	/* not same tuple */

					/*
					 * Found a match, but can't use it if it belongs to
					 * another list already
					 */
					if (ct->c_list)
						continue;

					found = true;
					break;		/* A-OK */
				}

				if (!found)
				{
					/* We didn't find a usable entry, so make a new one */
					ct = CatalogCacheCreateEntry(cache, ntp, NULL,
												 hashValue, hashIndex);

					/* upon failure, we must start the scan over */
					if (ct == NULL)
					{
						in_progress_ent.dead = true;
						break;
					}
				}

				/* Careful here: add entry to ctlist, then bump its refcount */
				/* This way leaves state correct if lappend runs out of memory */
				ctlist = lappend(ctlist, ct);
				ct->refcount++;
			}

			systable_endscan(scandesc);
		} while (in_progress_ent.dead);

		table_close(relation, AccessShareLock);

		/* Make sure the resource owner has room to remember this entry. */
		ResourceOwnerEnlarge(CurrentResourceOwner);

		/* Now we can build the CatCList entry. */
		oldcxt = MemoryContextSwitchTo(CacheMemoryContext);
		nmembers = list_length(ctlist);
		cl = (CatCList *)
			palloc(offsetof(CatCList, members) + nmembers * sizeof(CatCTup *));

		/* Extract key values */
		CatCacheCopyKeys(cache->cc_tupdesc, nkeys, cache->cc_keyno,
						 arguments, cl->keys);
		MemoryContextSwitchTo(oldcxt);

		/*
		 * We are now past the last thing that could trigger an elog before we
		 * have finished building the CatCList and remembering it in the
		 * resource owner.  So it's OK to fall out of the PG_TRY, and indeed
		 * we'd better do so before we start marking the members as belonging
		 * to the list.
		 */
	}
	PG_CATCH();
	{
		Assert(catcache_in_progress_stack == &in_progress_ent);
		catcache_in_progress_stack = save_in_progress;

		foreach(ctlist_item, ctlist)
		{
			ct = (CatCTup *) lfirst(ctlist_item);
			Assert(ct->c_list == NULL);
			Assert(ct->refcount > 0);
			ct->refcount--;
			if (
#ifndef CATCACHE_FORCE_RELEASE
				ct->dead &&
#endif
				ct->refcount == 0 &&
				(ct->c_list == NULL || ct->c_list->refcount == 0))
				CatCacheRemoveCTup(cache, ct);
		}

		PG_RE_THROW();
	}
	PG_END_TRY();
	Assert(catcache_in_progress_stack == &in_progress_ent);
	catcache_in_progress_stack = save_in_progress;

	cl->cl_magic = CL_MAGIC;
	cl->my_cache = cache;
	cl->refcount = 0;			/* for the moment */
	cl->dead = false;
	cl->ordered = ordered;
	cl->nkeys = nkeys;
	cl->hash_value = lHashValue;
	cl->n_members = nmembers;

	i = 0;
	foreach(ctlist_item, ctlist)
	{
		cl->members[i++] = ct = (CatCTup *) lfirst(ctlist_item);
		Assert(ct->c_list == NULL);
		ct->c_list = cl;
		/* release the temporary refcount on the member */
		Assert(ct->refcount > 0);
		ct->refcount--;
		/* mark list dead if any members already dead */
		if (ct->dead)
			cl->dead = true;
	}
	Assert(i == nmembers);

	/*
	 * Add the CatCList to the appropriate bucket, and count it.
	 */
	dlist_push_head(lbucket, &cl->cache_elem);

	cache->cc_nlist++;

	/* Finally, bump the list's refcount and return it */
	cl->refcount++;
	ResourceOwnerRememberCatCacheListRef(CurrentResourceOwner, cl);

	CACHE_elog(DEBUG2, "SearchCatCacheList(%s): made list of %d members",
			   cache->cc_relname, nmembers);

	return cl;
}

/*
 *	ReleaseCatCacheList
 *
 *	Decrement the reference count of a catcache list.
 */
void
ReleaseCatCacheList(CatCList *list)
{
	ReleaseCatCacheListWithOwner(list, CurrentResourceOwner);
}

static void
ReleaseCatCacheListWithOwner(CatCList *list, ResourceOwner resowner)
{
	/* Safety checks to ensure we were handed a cache entry */
	Assert(list->cl_magic == CL_MAGIC);
	Assert(list->refcount > 0);
	list->refcount--;
	if (resowner)
		ResourceOwnerForgetCatCacheListRef(resowner, list);

	if (
#ifndef CATCACHE_FORCE_RELEASE
		list->dead &&
#endif
		list->refcount == 0)
		CatCacheRemoveCList(list->my_cache, list);
}


/*
 * CatalogCacheCreateEntry
 *		Create a new CatCTup entry, copying the given HeapTuple and other
 *		supplied data into it.  The new entry initially has refcount 0.
 *
 * To create a normal cache entry, ntp must be the HeapTuple just fetched
 * from scandesc, and "arguments" is not used.  To create a negative cache
 * entry, pass NULL for ntp; then "arguments" is the cache keys to use.
 * In either case, hashValue/hashIndex are the hash values computed from
 * the cache keys.
 *
 * Returns NULL if we attempt to detoast the tuple and observe that it
 * became stale.  (This cannot happen for a negative entry.)  Caller must
 * retry the tuple lookup in that case.
 */
static CatCTup *
CatalogCacheCreateEntry(CatCache *cache, HeapTuple ntp, Datum *arguments,
						uint32 hashValue, Index hashIndex)
{
	CatCTup    *ct;
	MemoryContext oldcxt;

	if (ntp)
	{
		int			i;
		HeapTuple	dtp = NULL;

		/*
		 * The invalidation of the in-progress entry essentially never happens
		 * during our regression tests, and there's no easy way to force it to
		 * fail for testing purposes.  To ensure we have test coverage for the
		 * retry paths in our callers, make debug builds randomly fail about
		 * 0.1% of the times through this code path, even when there's no
		 * toasted fields.
		 */
#ifdef USE_ASSERT_CHECKING
		if (pg_prng_uint32(&pg_global_prng_state) <= (PG_UINT32_MAX / 1000))
			return NULL;
#endif

		/*
		 * If there are any out-of-line toasted fields in the tuple, expand
		 * them in-line.  This saves cycles during later use of the catcache
		 * entry, and also protects us against the possibility of the toast
		 * tuples being freed before we attempt to fetch them, in case of
		 * something using a slightly stale catcache entry.
		 */
		if (HeapTupleHasExternal(ntp))
		{
			CatCInProgress *save_in_progress;
			CatCInProgress in_progress_ent;

			/*
			 * The tuple could become stale while we are doing toast table
			 * access (since AcceptInvalidationMessages can run then).  The
			 * invalidation will mark our in-progress entry as dead.
			 */
			save_in_progress = catcache_in_progress_stack;
			in_progress_ent.next = catcache_in_progress_stack;
			in_progress_ent.cache = cache;
			in_progress_ent.hash_value = hashValue;
			in_progress_ent.list = false;
			in_progress_ent.dead = false;
			catcache_in_progress_stack = &in_progress_ent;

			PG_TRY();
			{
				dtp = toast_flatten_tuple(ntp, cache->cc_tupdesc);
			}
			PG_FINALLY();
			{
				Assert(catcache_in_progress_stack == &in_progress_ent);
				catcache_in_progress_stack = save_in_progress;
			}
			PG_END_TRY();

			if (in_progress_ent.dead)
			{
				heap_freetuple(dtp);
				return NULL;
			}
		}
		else
			dtp = ntp;

		/* Allocate memory for CatCTup and the cached tuple in one go */
		ct = (CatCTup *)
			MemoryContextAlloc(CacheMemoryContext,
							   MAXALIGN(sizeof(CatCTup)) + dtp->t_len);
		ct->tuple.t_len = dtp->t_len;
		ct->tuple.t_self = dtp->t_self;
		ct->tuple.t_tableOid = dtp->t_tableOid;
		ct->tuple.t_data = (HeapTupleHeader)
			(((char *) ct) + MAXALIGN(sizeof(CatCTup)));
		/* copy tuple contents */
		memcpy((char *) ct->tuple.t_data,
			   (const char *) dtp->t_data,
			   dtp->t_len);

		if (dtp != ntp)
			heap_freetuple(dtp);

		/* extract keys - they'll point into the tuple if not by-value */
		for (i = 0; i < cache->cc_nkeys; i++)
		{
			Datum		atp;
			bool		isnull;

			atp = heap_getattr(&ct->tuple,
							   cache->cc_keyno[i],
							   cache->cc_tupdesc,
							   &isnull);
			Assert(!isnull);
			ct->keys[i] = atp;
		}
	}
	else
	{
		/* Set up keys for a negative cache entry */
		oldcxt = MemoryContextSwitchTo(CacheMemoryContext);
		ct = palloc_object(CatCTup);

		/*
		 * Store keys - they'll point into separately allocated memory if not
		 * by-value.
		 */
		CatCacheCopyKeys(cache->cc_tupdesc, cache->cc_nkeys, cache->cc_keyno,
						 arguments, ct->keys);
		MemoryContextSwitchTo(oldcxt);
	}

	/*
	 * Finish initializing the CatCTup header, and add it to the cache's
	 * linked list and counts.
	 */
	ct->ct_magic = CT_MAGIC;
	ct->my_cache = cache;
	ct->c_list = NULL;
	ct->refcount = 0;			/* for the moment */
	ct->dead = false;
	ct->negative = (ntp == NULL);
	ct->hash_value = hashValue;

	dlist_push_head(&cache->cc_bucket[hashIndex], &ct->cache_elem);

	cache->cc_ntup++;
	CacheHdr->ch_ntup++;

	/*
	 * If the hash table has become too full, enlarge the buckets array. Quite
	 * arbitrarily, we enlarge when fill factor > 2.
	 */
	if (cache->cc_ntup > cache->cc_nbuckets * 2)
		RehashCatCache(cache);

	return ct;
}

/*
 * Helper routine that frees keys stored in the keys array.
 */
static void
CatCacheFreeKeys(TupleDesc tupdesc, int nkeys, const int *attnos, const Datum *keys)
{
	int			i;

	for (i = 0; i < nkeys; i++)
	{
		int			attnum = attnos[i];

		/* system attribute are not supported in caches */
		Assert(attnum > 0);

		if (!TupleDescCompactAttr(tupdesc, attnum - 1)->attbyval)
			pfree(DatumGetPointer(keys[i]));
	}
}

/*
 * Helper routine that copies the keys in the srckeys array into the dstkeys
 * one, guaranteeing that the datums are fully allocated in the current memory
 * context.
 */
static void
CatCacheCopyKeys(TupleDesc tupdesc, int nkeys, const int *attnos,
				 const Datum *srckeys, Datum *dstkeys)
{
	int			i;

	/*
	 * XXX: memory and lookup performance could possibly be improved by
	 * storing all keys in one allocation.
	 */

	for (i = 0; i < nkeys; i++)
	{
		int			attnum = attnos[i];
		Form_pg_attribute att = TupleDescAttr(tupdesc, attnum - 1);
		Datum		src = srckeys[i];
		NameData	srcname;

		/*
		 * Must be careful in case the caller passed a C string where a NAME
		 * is wanted: convert the given argument to a correctly padded NAME.
		 * Otherwise the memcpy() done by datumCopy() could fall off the end
		 * of memory.
		 */
		if (att->atttypid == NAMEOID)
		{
			namestrcpy(&srcname, DatumGetCString(src));
			src = NameGetDatum(&srcname);
		}

		dstkeys[i] = datumCopy(src,
							   att->attbyval,
							   att->attlen);
	}
}

/*
 *	PrepareToInvalidateCacheTuple()
 *
 *	This is part of a rather subtle chain of events, so pay attention:
 *
 *	When a tuple is inserted or deleted, it cannot be flushed from the
 *	catcaches immediately, for reasons explained at the top of cache/inval.c.
 *	Instead we have to add entry(s) for the tuple to a list of pending tuple
 *	invalidations that will be done at the end of the command or transaction.
 *
 *	The lists of tuples that need to be flushed are kept by inval.c.  This
 *	routine is a helper routine for inval.c.  Given a tuple belonging to
 *	the specified relation, find all catcaches it could be in, compute the
 *	correct hash value for each such catcache, and call the specified
 *	function to record the cache id and hash value in inval.c's lists.
 *	SysCacheInvalidate will be called later, if appropriate,
 *	using the recorded information.
 *
 *	For an insert or delete, tuple is the target tuple and newtuple is NULL.
 *	For an update, we are called just once, with tuple being the old tuple
 *	version and newtuple the new version.  We should make two list entries
 *	if the tuple's hash value changed, but only one if it didn't.
 *
 *	Note that it is irrelevant whether the given tuple is actually loaded
 *	into the catcache at the moment.  Even if it's not there now, it might
 *	be by the end of the command, or there might be a matching negative entry
 *	to flush --- or other backends' caches might have such entries --- so
 *	we have to make list entries to flush it later.
 *
 *	Also note that it's not an error if there are no catcaches for the
 *	specified relation.  inval.c doesn't know exactly which rels have
 *	catcaches --- it will call this routine for any tuple that's in a
 *	system relation.
 */
void
PrepareToInvalidateCacheTuple(Relation relation,
							  HeapTuple tuple,
							  HeapTuple newtuple,
							  void (*function) (int, uint32, Oid, void *),
							  void *context)
{
	slist_iter	iter;
	Oid			reloid;

	CACHE_elog(DEBUG2, "PrepareToInvalidateCacheTuple: called");

	/*
	 * sanity checks
	 */
	Assert(RelationIsValid(relation));
	Assert(HeapTupleIsValid(tuple));
	Assert(function);
	Assert(CacheHdr != NULL);

	reloid = RelationGetRelid(relation);

	/* ----------------
	 *	for each cache
	 *	   if the cache contains tuples from the specified relation
	 *		   compute the tuple's hash value(s) in this cache,
	 *		   and call the passed function to register the information.
	 * ----------------
	 */

	slist_foreach(iter, &CacheHdr->ch_caches)
	{
		CatCache   *ccp = slist_container(CatCache, cc_next, iter.cur);
		uint32		hashvalue;
		Oid			dbid;

		if (ccp->cc_reloid != reloid)
			continue;

		/* Just in case cache hasn't finished initialization yet... */
		ConditionalCatalogCacheInitializeCache(ccp);

		hashvalue = CatalogCacheComputeTupleHashValue(ccp, ccp->cc_nkeys, tuple);
		dbid = ccp->cc_relisshared ? (Oid) 0 : MyDatabaseId;

		(*function) (ccp->id, hashvalue, dbid, context);

		if (newtuple)
		{
			uint32		newhashvalue;

			newhashvalue = CatalogCacheComputeTupleHashValue(ccp, ccp->cc_nkeys, newtuple);

			if (newhashvalue != hashvalue)
				(*function) (ccp->id, newhashvalue, dbid, context);
		}
	}
}

/* ResourceOwner callbacks */

static void
ResOwnerReleaseCatCache(Datum res)
{
	ReleaseCatCacheWithOwner((HeapTuple) DatumGetPointer(res), NULL);
}

static char *
ResOwnerPrintCatCache(Datum res)
{
	HeapTuple	tuple = (HeapTuple) DatumGetPointer(res);
	CatCTup    *ct = (CatCTup *) (((char *) tuple) -
								  offsetof(CatCTup, tuple));

	/* Safety check to ensure we were handed a cache entry */
	Assert(ct->ct_magic == CT_MAGIC);

	return psprintf("cache %s (%d), tuple %u/%u has count %d",
					ct->my_cache->cc_relname, ct->my_cache->id,
					ItemPointerGetBlockNumber(&(tuple->t_self)),
					ItemPointerGetOffsetNumber(&(tuple->t_self)),
					ct->refcount);
}

static void
ResOwnerReleaseCatCacheList(Datum res)
{
	ReleaseCatCacheListWithOwner((CatCList *) DatumGetPointer(res), NULL);
}

static char *
ResOwnerPrintCatCacheList(Datum res)
{
	CatCList   *list = (CatCList *) DatumGetPointer(res);

	return psprintf("cache %s (%d), list %p has count %d",
					list->my_cache->cc_relname, list->my_cache->id,
					list, list->refcount);
}

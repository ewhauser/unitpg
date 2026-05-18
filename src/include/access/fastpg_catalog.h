/*-------------------------------------------------------------------------
 *
 * fastpg_catalog.h
 *	  Rust-backed virtual catalog boundary for fastpg builds.
 *
 *-------------------------------------------------------------------------
 */
#ifndef FASTPG_CATALOG_H
#define FASTPG_CATALOG_H

#include "postgres.h"

#ifdef USE_FASTPG

#include <stdint.h>

#define FASTPG_PROC_MAX_ARGS 8
#define FASTPG_PROC_SOURCE_LEN 64
#define FASTPG_MAX_INDEX_KEYS 32
#define FASTPG_VIRTUAL_CATALOG_STATIC 1
#define FASTPG_VIRTUAL_CATALOG_DYNAMIC 2
#define FASTPG_VIRTUAL_CATALOG_EMPTY 3

#ifndef INTEGER_BTREE_FAM_OID
#define INTEGER_BTREE_FAM_OID 1976
#endif
#ifndef OID_BTREE_FAM_OID
#define OID_BTREE_FAM_OID 1989
#endif
#ifndef TEXT_BTREE_FAM_OID
#define TEXT_BTREE_FAM_OID 1994
#endif

typedef struct FastPgRustCatalogType
{
	uint32_t	oid;
	uint32_t	namespace_oid;
	uint32_t	owner_oid;
	char		name[NAMEDATALEN];
	int16		typlen;
	uint8_t		typbyval;
	uint8_t		typalign;
	uint8_t		typdelim;
	uint8_t		_padding;
	uint32_t	typinput;
	uint32_t	typoutput;
	uint32_t	typreceive;
	uint32_t	typsend;
	uint32_t	typmodin;
	uint32_t	typmodout;
	uint8_t		typisdefined;
	uint8_t		typtype;
	uint8_t		typcategory;
	uint8_t		typispreferred;
	uint32_t	typrelid;
	uint32_t	typelem;
	uint32_t	typarray;
	uint32_t	typbasetype;
	int32		typtypmod;
	uint32_t	typcollation;
	uint32_t	typsubscript;
	uint8_t		typstorage;
	uint8_t		_trailing_padding[3];
} FastPgRustCatalogType;

typedef struct FastPgRustCatalogRelation
{
	uint32_t	oid;
	uint32_t	namespace_oid;
	char		name[NAMEDATALEN];
	uint16_t	column_count;
	uint8_t		relkind;
	uint8_t		has_primary_key;
} FastPgRustCatalogRelation;

typedef struct FastPgRustCatalogColumn
{
	char		name[NAMEDATALEN];
	uint32_t	type_oid;
	int32_t		type_mod;
	uint8_t		is_not_null;
	uint8_t		_padding[3];
} FastPgRustCatalogColumn;

typedef struct FastPgRustPrimaryKeyIndexInfo
{
	uint32_t	index_oid;
	uint32_t	heap_oid;
	uint16_t	key_count;
	uint8_t		_padding[2];
	int16_t		attnums[FASTPG_MAX_INDEX_KEYS];
	uint32_t	type_oids[FASTPG_MAX_INDEX_KEYS];
	uint32_t	collation_oids[FASTPG_MAX_INDEX_KEYS];
} FastPgRustPrimaryKeyIndexInfo;

typedef struct FastPgRustCatalogNamespace
{
	uint32_t	oid;
	uint32_t	owner_oid;
	char		name[NAMEDATALEN];
} FastPgRustCatalogNamespace;

typedef struct FastPgRustCatalogProc
{
	uint32_t	oid;
	uint32_t	namespace_oid;
	uint32_t	owner_oid;
	uint32_t	language_oid;
	char		name[NAMEDATALEN];
	char		source[FASTPG_PROC_SOURCE_LEN];
	float4		cost;
	float4		rows;
	uint32_t	variadic_oid;
	uint32_t	support_oid;
	uint32_t	return_type_oid;
	uint16_t	arg_count;
	uint16_t	arg_default_count;
	uint8_t		kind;
	uint8_t		security_definer;
	uint8_t		leakproof;
	uint8_t		is_strict;
	uint8_t		returns_set;
	uint8_t		volatility;
	uint8_t		parallel;
	uint8_t		_padding;
	uint32_t	arg_type_oids[FASTPG_PROC_MAX_ARGS];
} FastPgRustCatalogProc;

typedef struct FastPgRustCatalogAggregate
{
	uint32_t	function_oid;
	uint32_t	transition_fn_oid;
	uint32_t	final_fn_oid;
	uint32_t	combine_fn_oid;
	uint32_t	serial_fn_oid;
	uint32_t	deserial_fn_oid;
	uint32_t	moving_transition_fn_oid;
	uint32_t	moving_inverse_fn_oid;
	uint32_t	moving_final_fn_oid;
	uint32_t	sort_operator_oid;
	uint32_t	transition_type_oid;
	uint32_t	moving_transition_type_oid;
	int32_t		transition_space;
	int32_t		moving_transition_space;
	uint16_t	direct_arg_count;
	uint8_t		kind;
	uint8_t		final_extra;
	uint8_t		moving_final_extra;
	uint8_t		final_modify;
	uint8_t		moving_final_modify;
	uint8_t		has_init_value;
	uint8_t		has_moving_init_value;
} FastPgRustCatalogAggregate;

typedef struct FastPgRustCatalogOperator
{
	uint32_t	oid;
	uint32_t	namespace_oid;
	uint32_t	owner_oid;
	char		name[NAMEDATALEN];
	uint8_t		kind;
	uint8_t		can_merge;
	uint8_t		can_hash;
	uint8_t		_padding;
	uint32_t	left_type_oid;
	uint32_t	right_type_oid;
	uint32_t	result_type_oid;
	uint32_t	commutator_oid;
	uint32_t	negator_oid;
	uint32_t	code_fn_oid;
	uint32_t	rest_fn_oid;
	uint32_t	join_fn_oid;
} FastPgRustCatalogOperator;

typedef struct FastPgRustCatalogCast
{
	uint32_t	oid;
	uint32_t	source_type_oid;
	uint32_t	target_type_oid;
	uint32_t	function_oid;
	uint8_t		context;
	uint8_t		method;
	uint8_t		_padding[2];
} FastPgRustCatalogCast;

typedef struct FastPgRustCatalogOpclass
{
	uint32_t	oid;
	uint32_t	method_oid;
	uint32_t	namespace_oid;
	uint32_t	owner_oid;
	uint32_t	family_oid;
	uint32_t	input_type_oid;
	uint32_t	key_type_oid;
	uint8_t		is_default;
	uint8_t		_padding[3];
	char		name[NAMEDATALEN];
} FastPgRustCatalogOpclass;

extern bool fastpg_rust_catalog_type_by_oid(uint32_t oid,
											FastPgRustCatalogType *out);
extern bool fastpg_rust_catalog_type_by_name(const char *name,
											 uint32_t namespace_oid,
											 FastPgRustCatalogType *out);
extern uint8_t fastpg_rust_catalog_policy_by_relation_oid(uint32_t relation_oid);
extern bool fastpg_rust_catalog_relation_oid_by_name(const char *name,
													 uint32_t namespace_oid,
													 uint32_t *oid_out);
extern bool fastpg_rust_catalog_relation_by_oid(uint32_t oid,
												FastPgRustCatalogRelation *out);
extern bool fastpg_rust_catalog_relation_column_by_index(uint32_t relation_oid,
														 size_t column_index,
														 FastPgRustCatalogColumn *out);
extern bool fastpg_rust_catalog_primary_key_index_info(uint32_t index_oid,
													   FastPgRustPrimaryKeyIndexInfo *out);
extern bool fastpg_rust_catalog_primary_key_index_oid(uint32_t relation_oid,
													  uint32_t *oid_out);
extern bool fastpg_rust_catalog_namespace_by_oid(uint32_t oid,
												 FastPgRustCatalogNamespace *out);
extern bool fastpg_rust_catalog_namespace_by_name(const char *name,
												  FastPgRustCatalogNamespace *out);
extern bool fastpg_rust_catalog_proc_by_oid(uint32_t oid,
											FastPgRustCatalogProc *out);
extern size_t fastpg_rust_catalog_proc_count_by_name(const char *name);
extern bool fastpg_rust_catalog_proc_by_name_index(const char *name,
												   size_t index,
												   FastPgRustCatalogProc *out);
extern bool fastpg_rust_catalog_aggregate_by_proc_oid(uint32_t function_oid,
													  FastPgRustCatalogAggregate *out);
extern bool fastpg_rust_catalog_aggregate_init_value(uint32_t function_oid,
													 bool moving,
													 char *out,
													 size_t out_len);
extern bool fastpg_rust_catalog_operator_by_oid(uint32_t oid,
												FastPgRustCatalogOperator *out);
extern bool fastpg_rust_catalog_operator_by_signature(const char *name,
													  uint32_t left_type_oid,
													  uint32_t right_type_oid,
													  uint32_t namespace_oid,
													  FastPgRustCatalogOperator *out);
extern size_t fastpg_rust_catalog_operator_count_by_name(const char *name);
extern bool fastpg_rust_catalog_operator_by_name_index(const char *name,
													   size_t index,
													   FastPgRustCatalogOperator *out);
extern bool fastpg_rust_catalog_cast_by_source_target(uint32_t source_type_oid,
													  uint32_t target_type_oid,
													  FastPgRustCatalogCast *out);
extern bool fastpg_rust_catalog_opclass_by_oid(uint32_t oid,
											   FastPgRustCatalogOpclass *out);
extern bool fastpg_rust_catalog_opclass_by_name(uint32_t method_oid,
												const char *name,
												uint32_t namespace_oid,
												FastPgRustCatalogOpclass *out);
extern bool fastpg_rust_catalog_btree_opclass_for_type(uint32_t type_oid,
													   uint32_t *oid_out);
extern bool fastpg_rust_catalog_create_relation(const char *name,
												const char **column_names,
												const uint32_t *type_oids,
												const int32_t *type_mods,
												const uint8_t *not_nulls,
												size_t column_count,
												bool if_not_exists,
												char *sqlstate_out,
												size_t sqlstate_len,
												char *message_out,
												size_t message_len);
extern bool fastpg_rust_catalog_drop_relation(const char *name,
											  bool missing_ok,
											  char *sqlstate_out,
											  size_t sqlstate_len,
											  char *message_out,
											  size_t message_len);
extern bool fastpg_rust_catalog_truncate_relation(const char *name,
												  char *sqlstate_out,
												  size_t sqlstate_len,
												  char *message_out,
												  size_t message_len);
extern bool fastpg_rust_catalog_relation_column_count(const char *name,
													  size_t *count_out,
													  char *sqlstate_out,
													  size_t sqlstate_len,
													  char *message_out,
													  size_t message_len);
extern bool fastpg_rust_catalog_add_primary_key(const char *name,
												const char **column_names,
												size_t column_count,
												char *sqlstate_out,
												size_t sqlstate_len,
												char *message_out,
												size_t message_len);
extern bool fastpg_rust_catalog_create_type(const char *name,
											uint8_t kind,
											uint32_t subtype_oid,
											uint32_t collation_oid,
											const char **labels,
											size_t label_count,
											const char **column_names,
											const uint32_t *column_type_oids,
											const int32_t *column_type_mods,
											size_t column_count,
											char *sqlstate_out,
											size_t sqlstate_len,
											char *message_out,
											size_t message_len);
extern bool fastpg_rust_catalog_create_function(const char *name,
												uint32_t return_type_oid,
												const uint32_t *arg_type_oids,
												size_t arg_count,
												uint32_t language_oid,
												uint8_t is_strict,
												uint8_t leakproof,
												uint8_t returns_set,
												uint8_t volatility,
												uint8_t parallel,
												const char *source,
												char *sqlstate_out,
												size_t sqlstate_len,
												char *message_out,
												size_t message_len);
extern bool fastpg_rust_catalog_create_opclass(const char *name,
											  const char *method_name,
											  uint32_t input_type_oid,
											  uint8_t is_default,
											  char *sqlstate_out,
											  size_t sqlstate_len,
											  char *message_out,
											  size_t message_len);
extern void fastpg_rust_xact_begin(void);
extern void fastpg_rust_xact_begin_implicit(void);
extern void fastpg_rust_xact_commit(void);
extern void fastpg_rust_xact_abort(void);
extern void fastpg_rust_xact_commit_if_implicit(void);
extern void fastpg_rust_xact_abort_if_implicit(void);
extern bool fastpg_rust_xact_is_explicit(void);
extern void fastpg_rust_subxact_begin(void);
extern void fastpg_rust_subxact_commit(void);
extern void fastpg_rust_subxact_abort(void);

#endif							/* USE_FASTPG */

#endif							/* FASTPG_CATALOG_H */

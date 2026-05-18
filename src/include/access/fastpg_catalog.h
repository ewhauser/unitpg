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

typedef struct FastPgRustCatalogType
{
	uint32_t	oid;
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

extern bool fastpg_rust_catalog_type_by_oid(uint32_t oid,
											FastPgRustCatalogType *out);
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
extern void fastpg_rust_xact_begin(void);
extern void fastpg_rust_xact_commit(void);
extern void fastpg_rust_xact_abort(void);
extern void fastpg_rust_subxact_begin(void);
extern void fastpg_rust_subxact_commit(void);
extern void fastpg_rust_subxact_abort(void);

#endif							/* USE_FASTPG */

#endif							/* FASTPG_CATALOG_H */

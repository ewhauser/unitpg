# src/test/modules/fastpg_parser_probe/Makefile

MODULE_big = fastpg_parser_probe
OBJS = \
	$(WIN32RES) \
	fastpg_parser_ffi.o \
	fastpg_parser_probe.o
PGFILEDESC = "fastpg_parser_probe - test code for fastpg parser reuse"

EXTENSION = fastpg_parser_probe
DATA = fastpg_parser_probe--1.0.sql

REGRESS = fastpg_parser_probe

ifdef USE_PGXS
PG_CONFIG = pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)
else
subdir = src/test/modules/fastpg_parser_probe
top_builddir = ../../../..
include $(top_builddir)/src/Makefile.global
include $(top_srcdir)/contrib/contrib-global.mk
endif

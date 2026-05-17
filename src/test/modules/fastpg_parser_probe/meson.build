# Copyright (c) 2022-2026, PostgreSQL Global Development Group

fastpg_parser_probe_sources = files(
  'fastpg_parser_ffi.c',
  'fastpg_parser_probe.c',
)

if host_system == 'windows'
  fastpg_parser_probe_sources += rc_lib_gen.process(win32ver_rc, extra_args: [
    '--NAME', 'fastpg_parser_probe',
    '--FILEDESC', 'fastpg_parser_probe - test code for fastpg parser reuse',])
endif

fastpg_parser_probe = shared_module('fastpg_parser_probe',
  fastpg_parser_probe_sources,
  kwargs: pg_test_mod_args,
)
test_install_libs += fastpg_parser_probe

test_install_data += files(
  'fastpg_parser_probe.control',
  'fastpg_parser_probe--1.0.sql',
)

tests += {
  'name': 'fastpg_parser_probe',
  'sd': meson.current_source_dir(),
  'bd': meson.current_build_dir(),
  'regress': {
    'sql': [
      'fastpg_parser_probe',
    ],
  },
}

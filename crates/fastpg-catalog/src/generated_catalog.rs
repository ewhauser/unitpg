#![allow(unused_imports)]

use fastpg_types::Oid;

use crate::model::*;

include!(concat!(env!("OUT_DIR"), "/generated_static_catalog.rs"));

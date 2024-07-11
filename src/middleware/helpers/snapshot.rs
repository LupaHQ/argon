use rbx_dom_weak::{types::Ref, Instance, WeakDom};
use std::collections::HashMap;

use crate::core::{meta::Meta, snapshot::Snapshot};

// Based on Rojo's InstanceSnapshot::from_tree (https://github.com/rojo-rbx/rojo/blob/master/src/snapshot/instance_snapshot.rs#L105)
pub fn snapshot_from_dom(dom: WeakDom, id: Ref) -> Snapshot {
	let (_, mut raw_dom) = dom.into_raw();

	fn walk(id: Ref, raw_dom: &mut HashMap<Ref, Instance>) -> Snapshot {
		let instance = raw_dom
			.remove(&id)
			.expect("Provided ID does not exist in the current DOM");

		let children = instance
			.children()
			.iter()
			.map(|&child_id| walk(child_id, raw_dom))
			.collect();

		let mut meta = Meta::new();

		if instance.class == "MeshPart" {
			meta.mesh_source = super::save_mesh(&instance.properties);
		}

		Snapshot::new()
			.with_name(&instance.name)
			.with_class(&instance.class)
			.with_properties(instance.properties)
			.with_children(children)
	}

	walk(id, &mut raw_dom)
}

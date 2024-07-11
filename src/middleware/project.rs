use anyhow::{bail, Result};
use colored::Colorize;
use log::error;
use path_clean::PathClean;
use rbx_dom_weak::types::Tags;
use std::{collections::HashMap, path::Path};

use super::new_snapshot;
use crate::{
	argon_warn,
	core::{
		meta::{Context, Meta, NodePath, Source},
		snapshot::Snapshot,
	},
	ext::PathExt,
	middleware::helpers,
	project::{Project, ProjectNode},
	util,
	vfs::Vfs,
};

#[profiling::function]
pub fn read_project(path: &Path, vfs: &Vfs) -> Result<Snapshot> {
	let project: Project = Project::load(path)?;

	let meta = Meta::from_project(&project);
	let mut snapshot = new_snapshot_node(&project.name, path, project.node, NodePath::new(), &meta.context, vfs)?;

	let mut source = Source::file(path).with_relevants(snapshot.meta.source.relevant().to_owned());
	source.add_project(path);

	snapshot.set_meta(meta.with_source(source));

	vfs.watch(path, false)?;

	Ok(snapshot)
}

#[profiling::function]
pub fn new_snapshot_node(
	name: &str,
	path: &Path,
	node: ProjectNode,
	node_path: NodePath,
	context: &Context,
	vfs: &Vfs,
) -> Result<Snapshot> {
	if node.class_name.is_some() && node.path.is_some() {
		bail!("Failed to load project: $className and $path cannot be set at the same time");
	}

	let class = {
		if let Some(class_name) = &node.class_name {
			class_name.to_owned()
		} else if util::is_service(name) {
			name.to_owned()
		} else {
			String::from("Folder")
		}
	};

	let mut properties = {
		let mut properties = HashMap::new();

		for (property, value) in &node.properties {
			match value.clone().resolve(&class, property) {
				Ok(value) => {
					properties.insert(property.to_owned(), value);
				}
				Err(err) => {
					error!("Failed to parse property: {}", err);
				}
			}
		}

		if let Some(attributes) = &node.attributes {
			match attributes.clone().resolve(&class, "Attributes") {
				Ok(value) => {
					properties.insert(String::from("Attributes"), value);
				}
				Err(err) => {
					error!("Failed to parse attributes: {}", err);
				}
			}
		}

		if !node.tags.is_empty() {
			properties.insert(String::from("Tags"), Tags::from(node.tags.clone()).into());
		}

		properties
	};

	let mut meta = Meta::new()
		.with_source(Source::project(name, path, node.clone(), node_path.clone()))
		.with_context(context)
		.with_keep_unknowns(node.keep_unknowns.unwrap_or_else(|| util::is_service(&class)));

	if class == "MeshPart" {
		meta.mesh_source = helpers::save_mesh(&mut properties);
	}

	let mut snapshot = Snapshot::new()
		.with_name(name)
		.with_class(&class)
		.with_properties(properties)
		.with_meta(meta);

	if let Some(custom_path) = node.path {
		let path = path.with_file_name(custom_path).clean();

		if let Some(mut path_snapshot) = new_snapshot(&path, context, vfs)? {
			path_snapshot.extend_properties(snapshot.properties);
			path_snapshot.set_name(&snapshot.name);

			if path_snapshot.class == "Folder" {
				path_snapshot.set_class(&snapshot.class);
			}

			// We want to keep the original inner source
			// but with addition of new relevant paths
			snapshot
				.meta
				.source
				.extend_relavants(path_snapshot.meta.source.relevant().to_owned());

			path_snapshot.meta.source = snapshot.meta.source;
			path_snapshot.meta.keep_unknowns = path_snapshot.meta.keep_unknowns || snapshot.meta.keep_unknowns;

			snapshot = path_snapshot;

			vfs.watch(&path, vfs.is_dir(&path))?;
		} else {
			argon_warn!(
				"Path specified in the project does not exist: {}. Please create this path and restart Argon \
				to watch for file changes in this path or remove it from the project to suppress this warning",
				path.to_string().bold()
			);
		}
	}

	for (node_name, node) in node.tree {
		let node_path = node_path.join(&node_name);
		let child = new_snapshot_node(&node_name, path, node, node_path, context, vfs)?;

		snapshot.add_child(child);
	}

	Ok(snapshot)
}

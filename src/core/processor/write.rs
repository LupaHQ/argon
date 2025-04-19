use anyhow::{anyhow, Context as AnyhowContext, Result};
use log::{error, trace, warn};
use path_clean::PathClean;
use rbx_dom_weak::{types::Ref, HashMapExt, Instance, Ustr, UstrMap};
use std::path::{Path, PathBuf};

use crate::{
	config::Config,
	core::{
		helpers::syncback::{rename_path, serialize_properties, validate_properties, verify_name, verify_path},
		meta::{Meta, NodePath, Source, SourceEntry, SourceKind},
		snapshot::{AddedSnapshot, Snapshot, UpdatedSnapshot},
		tree::Tree,
	},
	ext::PathExt,
	middleware::{
		data::{self, write_original_name},
		dir, Middleware,
	},
	project::{Project, ProjectNode},
	vfs::Vfs,
	Properties,
};

macro_rules! filter_warn {
	($id:expr) => {
		warn!("Instance {} does not pass syncback filter! Skipping..", $id);
	};
	($id:expr, $path:expr) => {
		warn!(
			"Path: {} (source of instance: {}) does not pass syncback filter! Skipping..",
			$path.display(),
			$id
		);
	};
}

pub fn apply_addition(snapshot: AddedSnapshot, tree: &mut Tree, vfs: &Vfs) -> Result<()> {
	trace!(
		"Adding Ref({:?}) '{}' [{}] with parent Ref({:?})",
		snapshot.id,
		snapshot.name,
		snapshot.class,
		snapshot.parent
	);

	if !tree.exists(snapshot.parent) {
		warn!(
			"Attempted to add instance: {:?} whose parent doesn't exist: {:?}",
			snapshot.id, snapshot.parent
		);
		return Ok(());
	}

	let parent_id = snapshot.parent;
	let mut snapshot = Snapshot::from(snapshot);
	let parent_instance = tree.get_instance(parent_id).unwrap();
	let mut parent_meta = tree.get_meta(parent_id).unwrap().clone();
	let filter = parent_meta.context.syncback_filter();

	trace!(
		"Parent Instance: ID={:?}, Name='{}', Class='{}'",
		parent_id,
		parent_instance.name,
		parent_instance.class
	);
	trace!("Parent Meta Source: {:?}", parent_meta.source);
	if let Some(path) = parent_meta.source.get().path() {
		trace!("Parent VFS Path Lookup: SUCCESS -> {}", path.display());
	} else {
		trace!("Parent VFS Path Lookup: FAILED -> No path found in source");
	}

	if filter.matches_name(&snapshot.name) || filter.matches_class(&snapshot.class) {
		filter_warn!(snapshot.id);
		return Ok(());
	}

	snapshot.properties = validate_properties(snapshot.properties, filter);

	fn locate_instance_data(is_dir: bool, path: &Path, snapshot: &Snapshot, parent_meta: &Meta) -> Result<PathBuf> {
		trace!(
			"locate_instance_data: Entering function with is_dir={}, path={}, snapshot_name={}",
			is_dir,
			path.display(),
			snapshot.name
		);
		let result = parent_meta
			.context
			.sync_rules_of_type(&Middleware::InstanceData, true)
			.iter()
			.find_map(|rule| {
				trace!("locate_instance_data: Checking rule: {:?}", rule);
				let located = rule.locate(path, &snapshot.name, is_dir);
				trace!("locate_instance_data: Rule locate result: {:?}", located);
				located
			})
			.with_context(|| format!("Failed to locate data path for parent: {}", path.display()));
		trace!("locate_instance_data: Result: {:?}", result);
		trace!("locate_instance_data: Exiting function");
		result
	}

	fn write_instance(
		has_children: bool,
		path: &mut PathBuf,
		snapshot: &mut Snapshot,
		parent_meta: &Meta,
		vfs: &Vfs,
	) -> Result<Option<Meta>> {
		trace!(
			"write_instance: Entering function with has_children={}, path={}, snapshot_name={}, snapshot_class={}",
			has_children,
			path.display(),
			snapshot.name,
			snapshot.class
		);
		let mut meta = snapshot.meta.clone().with_context(&parent_meta.context);
		let filter = parent_meta.context.syncback_filter();
		let mut properties = snapshot.properties.clone();

		trace!("write_instance: Initial meta: {:?}", meta);
		trace!("write_instance: Initial properties: {:?}", properties);

		if let Some(middleware) = Middleware::from_class(
			&snapshot.class,
			if !parent_meta.context.use_legacy_scripts() {
				trace!("write_instance: Using modern script handling");
				Some(&mut properties)
			} else {
				trace!("write_instance: Using legacy script handling");
				None
			},
		) {
			trace!("write_instance: Found middleware: {:?}", middleware);
			let mut file_path = parent_meta
				.context
				.sync_rules_of_type(&middleware, true)
				.iter()
				.find_map(|rule| {
					trace!("write_instance: Checking file sync rule: {:?}", rule);
					let located = rule.locate(path, &snapshot.name, has_children);
					trace!("write_instance: Rule locate result: {:?}", located);
					located
				})
				.with_context(|| format!("Failed to locate file path for parent: {}", path.display()))?;

			trace!("write_instance: Located file path: {}", file_path.display());

			if has_children {
				trace!("write_instance: Handling instance with children (directory like)");
				if filter.matches_path(path) {
					filter_warn!(snapshot.id, path);
					trace!("write_instance: Exiting function early (directory path filtered)");
					return Ok(None);
				}

				if !verify_path(path, &mut snapshot.name, &mut meta, vfs) {
					trace!("write_instance: Exiting function early (directory path verification failed)");
					return Ok(None);
				}

				trace!(
					"write_instance: Verified directory path: {}, updated name: {}, updated meta: {:?}",
					path.display(),
					snapshot.name,
					meta
				);

				dir::write_dir(path, vfs)?;

				trace!("write_instance: Wrote directory: {}", path.display());

				meta.set_source(Source::child_file(path, &file_path));
				trace!("write_instance: Set meta source to child_file: {:?}", meta.source);
			} else {
				trace!("write_instance: Handling instance without children (file like)");
				if !verify_path(&mut file_path, &mut snapshot.name, &mut meta, vfs) {
					trace!("write_instance: Exiting function early (file path verification failed)");
					return Ok(None);
				}
				trace!(
					"write_instance: Verified file path: {}, updated name: {}, updated meta: {:?}",
					file_path.display(),
					snapshot.name,
					meta
				);

				meta.set_source(Source::file(&file_path));
				trace!("write_instance: Set meta source to file: {:?}", meta.source);
			}

			if filter.matches_path(&file_path) {
				filter_warn!(snapshot.id, &file_path);
				trace!("write_instance: Exiting function early (file path filtered)");
				return Ok(None);
			}

			let properties = middleware.write(properties, &file_path, vfs)?;
			trace!(
				"write_instance: Middleware wrote to file: {}, remaining properties: {:?}",
				file_path.display(),
				properties
			);
			let data_path = locate_instance_data(has_children, path, snapshot, parent_meta)?;
			trace!("write_instance: Located data path: {}", data_path.display());

			if filter.matches_path(&data_path) {
				filter_warn!(snapshot.id, &data_path);
				trace!("write_instance: Data path filtered, skipping data write.");
			} else {
				let data_path = data::write_data(true, &snapshot.class, properties, &data_path, &meta, vfs)?;
				trace!("write_instance: Wrote data to path: {:?}", data_path);
				meta.source.set_data(data_path);
				trace!("write_instance: Updated meta source with data path: {:?}", meta.source);
			}
		} else {
			trace!(
				"write_instance: No specific middleware found for class: {}",
				snapshot.class
			);
			if filter.matches_path(path) {
				filter_warn!(snapshot.id, path);
				trace!("write_instance: Exiting function early (directory path filtered)");
				return Ok(None);
			}

			if !verify_path(path, &mut snapshot.name, &mut meta, vfs) {
				trace!("write_instance: Exiting function early (directory path verification failed)");
				return Ok(None);
			}

			trace!(
				"write_instance: Verified directory path: {}, updated name: {}, updated meta: {:?}",
				path.display(),
				snapshot.name,
				meta
			);

			dir::write_dir(path, vfs)?;

			trace!("write_instance: Wrote directory: {}", path.display());

			meta.set_source(Source::directory(path));
			trace!("write_instance: Set meta source to directory: {:?}", meta.source);

			let data_path = locate_instance_data(true, path, snapshot, parent_meta)?;
			trace!("write_instance: Located data path: {}", data_path.display());

			if filter.matches_path(&data_path) {
				filter_warn!(snapshot.id, &data_path);
				trace!("write_instance: Data path filtered, skipping data write.");
			} else {
				let data_path = data::write_data(false, &snapshot.class, properties, &data_path, &meta, vfs)?;
				trace!("write_instance: Wrote data to path: {:?}", data_path);
				meta.source.set_data(data_path);
				trace!("write_instance: Updated meta source with data path: {:?}", meta.source);
			}
		}

		trace!("write_instance: Exiting function successfully with meta: {:?}", meta);
		Ok(Some(meta))
	}

	fn add_non_project_instances(
		parent_id: Ref,
		parent_path: &Path,
		mut snapshot: Snapshot,
		parent_meta: &mut Meta,
		tree: &mut Tree,
		vfs: &Vfs,
	) -> Result<Source> {
		trace!(
			"add_non_project_instances: Entering function with parent_id={:?}, parent_path={}, snapshot_name={}",
			parent_id,
			parent_path.display(),
			snapshot.name
		);
		let config = Config::new();

		let mut parent_path = parent_path.to_owned();
		trace!(
			"add_non_project_instances: Initial parent_path: {}",
			parent_path.display()
		);

		// Transform parent instance source from file to folder
		let parent_source = if vfs.is_file(&parent_path) {
			trace!(
				"add_non_project_instances: Parent path {} is a file, transforming to folder source",
				parent_path.display()
			);
			let sync_rule = parent_meta
				.context
				.sync_rules()
				.iter()
				.filter(|rule| {
					if let Some(pattern) = rule.child_pattern.as_ref() {
						let skip = (pattern.as_str().starts_with(".src") || pattern.as_str().ends_with(".data.json"))
							&& config.rojo_mode;
						trace!(
							"add_non_project_instances: Filtering sync rule: {:?}, skip={}",
							rule,
							skip
						);
						!skip
					} else {
						true
					}
				})
				.find(|rule| {
					let matches = rule.matches(&parent_path);
					trace!(
						"add_non_project_instances: Checking sync rule {:?} against path {}: matches={}",
						rule,
						parent_path.display(),
						matches
					);
					matches
				})
				.with_context(|| format!("Failed to find sync rule for path: {}", parent_path.display()))?
				.clone();

			trace!("add_non_project_instances: Found sync rule: {:?}", sync_rule);

			let name = sync_rule.get_name(&parent_path);
			trace!("add_non_project_instances: Extracted name from sync rule: {}", name);
			let mut folder_path = parent_path.with_file_name(&name);
			trace!(
				"add_non_project_instances: Proposed folder path: {}",
				folder_path.display()
			);

			if !verify_path(&mut folder_path, &mut snapshot.name, parent_meta, vfs) {
				trace!(
					"add_non_project_instances: Folder path verification failed, returning original parent source: {:?}",
					parent_meta.source
				);
				return Ok(parent_meta.source.clone());
			}

			trace!(
				"add_non_project_instances: Verified folder path: {}, updated name: {}, updated meta: {:?}",
				folder_path.display(),
				snapshot.name,
				parent_meta
			);

			let file_path = sync_rule
				.locate(&folder_path, &name, true)
				.with_context(|| format!("Failed to locate file path for parent: {}", folder_path.display()))?;

			trace!(
				"add_non_project_instances: Located new file path within folder: {}",
				file_path.display()
			);

			let data_paths = if let Some(data) = parent_meta.source.get_data() {
				trace!(
					"add_non_project_instances: Found existing data path: {}",
					data.path().display()
				);
				let new_path = parent_meta
					.context
					.sync_rules_of_type(&Middleware::InstanceData, true)
					.iter()
					.find_map(|rule| rule.locate(&folder_path, &name, true))
					.with_context(|| format!("Failed to locate data path for parent: {}", folder_path.display()))?;

				trace!(
					"add_non_project_instances: Located new data path: {}",
					new_path.display()
				);
				Some((data.path().to_owned(), new_path))
			} else {
				trace!("add_non_project_instances: No existing data path found.");
				None
			};

			let mut source = Source::child_file(&folder_path, &file_path);
			trace!("add_non_project_instances: Created new child_file source: {:?}", source);

			dir::write_dir(&folder_path, vfs)?;
			trace!(
				"add_non_project_instances: Wrote new directory: {}",
				folder_path.display()
			);
			vfs.rename(&parent_path, &file_path)?;
			trace!(
				"add_non_project_instances: Renamed original file {} to {}",
				parent_path.display(),
				file_path.display()
			);

			if let Some(data_paths) = data_paths {
				trace!(
					"add_non_project_instances: Processing data path rename from {} to {}",
					data_paths.0.display(),
					data_paths.1.display()
				);
				source.add_data(&data_paths.1);
				vfs.rename(&data_paths.0, &data_paths.1)?;
				trace!(
					"add_non_project_instances: Renamed data file and updated source: {:?}",
					source
				);
			}

			parent_path = folder_path;
			trace!(
				"add_non_project_instances: Updated parent_path to new folder path: {}",
				parent_path.display()
			);

			source
		} else {
			trace!("add_non_project_instances: Parent path {} is already a directory or does not exist, using original source: {:?}", parent_path.display(), parent_meta.source);
			parent_meta.source.clone()
		};

		if !verify_name(&mut snapshot.name, &mut snapshot.meta) {
			trace!(
				"add_non_project_instances: Name verification failed for {}, returning parent source: {:?}",
				snapshot.name,
				parent_source
			);
			return Ok(parent_source);
		}

		trace!(
			"add_non_project_instances: Verified name: {}, updated meta: {:?}",
			snapshot.name,
			snapshot.meta
		);

		let mut path = parent_path.join(&snapshot.name);
		trace!("add_non_project_instances: Constructed child path: {}", path.display());

		if snapshot.children.is_empty() {
			trace!("add_non_project_instances: Snapshot has no children, writing as potential file");
			if let Some(meta) = write_instance(false, &mut path, &mut snapshot, parent_meta, vfs)? {
				trace!("add_non_project_instances: write_instance succeeded, meta: {:?}", meta);
				let snapshot_id = snapshot.id;
				let snapshot = snapshot.with_meta(meta);
				tree.insert_instance_with_ref(snapshot.clone(), parent_id);
				trace!(
					"add_non_project_instances: Inserted instance into tree: {:?}",
					snapshot_id
				);
			} else {
				trace!("add_non_project_instances: write_instance returned None, instance not added.");
			}
		} else if let Some(mut meta) = write_instance(true, &mut path, &mut snapshot, parent_meta, vfs)? {
			trace!("add_non_project_instances: Snapshot has children, writing as potential directory");
			trace!("add_non_project_instances: write_instance succeeded, meta: {:?}", meta);
			let snapshot_id = snapshot.id;
			let snapshot = snapshot.with_meta(meta.clone());

			tree.insert_instance_with_ref(snapshot.clone(), parent_id);
			trace!(
				"add_non_project_instances: Inserted instance into tree: {:?}",
				snapshot_id
			);

			for mut child in snapshot.children {
				trace!("add_non_project_instances: Processing child: {:?}", child.id);
				child.properties = validate_properties(child.properties.clone(), meta.context.syncback_filter());
				trace!(
					"add_non_project_instances: Validated child properties: {:?}",
					child.properties
				);
				add_non_project_instances(snapshot.id, &path, child, &mut meta, tree, vfs)?;
			}
		} else {
			trace!("add_non_project_instances: write_instance returned None, instance and children not added.");
		}

		trace!(
			"add_non_project_instances: Exiting function successfully, returning parent_source: {:?}",
			parent_source
		);
		Ok(parent_source)
	}

	fn add_project_instances(
		parent_id: Ref,
		path: &Path,
		node_path: NodePath,
		mut snapshot: Snapshot,
		parent_node: &mut ProjectNode,
		parent_meta: &Meta,
		tree: &mut Tree,
	) {
		trace!(
			"add_project_instances: Entering function with parent_id={:?}, path={}, node_path={:?}, snapshot_name={}",
			parent_id,
			path.display(),
			node_path,
			snapshot.name
		);
		let mut node = ProjectNode {
			class_name: Some(snapshot.class),
			properties: serialize_properties(&snapshot.class, snapshot.properties.clone()),
			..ProjectNode::default()
		};
		trace!("add_project_instances: Created initial project node: {:?}", node);

		if snapshot.meta.keep_unknowns {
			trace!("add_project_instances: Setting keep_unknowns=true on project node");
			node.keep_unknowns = Some(true);
		}

		let node_path = node_path.join(&snapshot.name);
		trace!("add_project_instances: Constructed new node_path: {:?}", node_path);
		let source = Source::project(&snapshot.name, path, node.clone(), node_path.clone());
		trace!("add_project_instances: Created project source: {:?}", source);
		let meta = snapshot
			.meta
			.clone()
			.with_context(&parent_meta.context)
			.with_source(source);
		trace!("add_project_instances: Created final meta: {:?}", meta);

		snapshot.meta = meta;
		tree.insert_instance_with_ref(snapshot.clone(), parent_id);
		trace!("add_project_instances: Inserted instance into tree: {:?}", snapshot.id);

		let filter = snapshot.meta.context.syncback_filter();
		trace!("add_project_instances: Syncback filter for children: {:?}", filter);

		for mut child in snapshot.children {
			trace!("add_project_instances: Processing child: {:?}", child.id);
			child.properties = validate_properties(child.properties, filter);
			trace!(
				"add_project_instances: Validated child properties: {:?}",
				child.properties
			);
			add_project_instances(parent_id, path, node_path.clone(), child, &mut node, parent_meta, tree);
		}

		parent_node.tree.insert(snapshot.name.clone(), node);
		trace!(
			"add_project_instances: Inserted node for {} into parent node's tree",
			snapshot.name
		);
		trace!("add_project_instances: Exiting function");
	}

	trace!(
		"apply_addition: Matching parent source kind: {:?}",
		parent_meta.source.get()
	);
	match parent_meta.source.get().clone() {
		SourceKind::Path(path) => {
			trace!("apply_addition: Parent source is Path: {}", path.display());
			let parent_source = add_non_project_instances(parent_id, &path, snapshot, &mut parent_meta, tree, vfs)?;
			trace!(
				"apply_addition: add_non_project_instances returned parent_source: {:?}",
				parent_source
			);

			parent_meta.set_source(parent_source);
			tree.update_meta(parent_id, parent_meta);
			trace!("apply_addition: Updated parent meta in tree with new source");
		}
		SourceKind::Project(name, path, node, node_path) => {
			trace!(
				"apply_addition: Parent source is Project: name={}, path={}, node_path={:?}",
				name,
				path.display(),
				node_path
			);
			if let Some(custom_path) = &node.path {
				trace!("apply_addition: Parent project node has custom path: {:?}", custom_path);
				let custom_path = path.with_file_name(custom_path.path()).clean();
				trace!("apply_addition: Resolved custom path: {}", custom_path.display());

				let parent_source =
					add_non_project_instances(parent_id, &custom_path, snapshot, &mut parent_meta, tree, vfs)?;
				trace!(
					"apply_addition: add_non_project_instances (for custom path) returned parent_source: {:?}",
					parent_source
				);

				let parent_source = Source::project(&name, &path, *node.clone(), node_path.clone())
					.with_relevant(parent_source.relevant().to_owned());
				trace!("apply_addition: Created new parent project source: {:?}", parent_source);

				parent_meta.set_source(parent_source);
				tree.update_meta(parent_id, parent_meta);
				trace!("apply_addition: Updated parent meta in tree with new project source");
			} else {
				trace!("apply_addition: Parent project node does not have custom path");
				let mut project = Project::load(&path)?;
				trace!("apply_addition: Loaded project from {}", path.display());

				let node = project
					.find_node_by_path(&node_path)
					.context(format!("Failed to find project node with path {:?}", node_path))?;
				trace!("apply_addition: Found parent project node: {:?}", node);

				add_project_instances(parent_id, &path, node_path.clone(), snapshot, node, &parent_meta, tree);

				project.save(&path)?;
				trace!("apply_addition: Saved project to {}", path.display());
			}
		}
		SourceKind::None => {
			let msg = format!(
				"apply_addition: Attempted to add instance {:?} whose parent {:?} has no source",
				snapshot.id, parent_id
			);
			error!("{}", msg);
			panic!("{}", msg);
		}
	}

	trace!("apply_addition: Exiting function successfully");
	Ok(())
}

pub fn apply_update(snapshot: UpdatedSnapshot, tree: &mut Tree, vfs: &Vfs) -> Result<()> {
	trace!("Updating {:?}", snapshot.id);

	if let Some(instance) = tree.get_instance(snapshot.id) {
		let filter = tree.get_meta(snapshot.id).unwrap().context.syncback_filter();
		trace!("apply_update: Instance {:?} exists. Filter: {:?}", snapshot.id, filter);

		if filter.matches_name(&instance.name) || filter.matches_class(&instance.class) {
			filter_warn!(snapshot.id);
			trace!("apply_update: Exiting function early (instance filtered by current name/class)");
			return Ok(());
		}

		if snapshot.name.as_ref().is_some_and(|name| filter.matches_name(name)) {
			filter_warn!(snapshot.id);
			trace!("apply_update: Exiting function early (instance filtered by new name)");
			return Ok(());
		}

		if snapshot.class.as_ref().is_some_and(|class| filter.matches_class(class)) {
			filter_warn!(snapshot.id);
			trace!("apply_update: Exiting function early (instance filtered by new class)");
			return Ok(());
		}
	} else {
		warn!(
			"apply_update: Attempted to update instance that doesn't exist: {:?}",
			snapshot.id
		);
		trace!("apply_update: Exiting function early (instance does not exist)");
		return Ok(());
	}

	let mut meta = tree.get_meta(snapshot.id).unwrap().clone();
	let instance = tree.get_instance_mut(snapshot.id).unwrap();
	trace!(
		"apply_update: Got meta: {:?} and mutable instance: {:?}",
		meta,
		instance
	);

	fn locate_instance_data(name: &str, path: &Path, meta: &Meta, vfs: &Vfs) -> Option<PathBuf> {
		trace!(
			"locate_instance_data (update): Entering function with name={}, path={}, meta={:?}",
			name,
			path.display(),
			meta
		);
		let data_path = if let Some(data) = meta.source.get_data() {
			trace!(
				"locate_instance_data (update): Found existing data path in meta: {}",
				data.path().display()
			);
			Some(data.path().to_owned())
		} else {
			trace!("locate_instance_data (update): No data path in meta, searching using sync rules");
			meta.context
				.sync_rules_of_type(&Middleware::InstanceData, true)
				.iter()
				.find_map(|rule| {
					trace!("locate_instance_data (update): Checking rule: {:?}", rule);
					let located = rule.locate(path, name, vfs.is_dir(path));
					trace!("locate_instance_data (update): Rule locate result: {:?}", located);
					located
				})
		};

		if data_path.is_none() {
			warn!(
				"locate_instance_data (update): Failed to locate instance data for {}",
				path.display()
			)
		}
		trace!("locate_instance_data (update): Result: {:?}", data_path);
		trace!("locate_instance_data (update): Exiting function");
		data_path
	}

	fn update_non_project_properties(
		path: &Path,
		properties: Properties,
		instance: &mut Instance,
		meta: &mut Meta,
		vfs: &Vfs,
	) -> Result<()> {
		trace!(
			"update_non_project_properties: Entering function with path={}, properties={:?}, instance={:?}, meta={:?}",
			path.display(),
			properties,
			instance.referent(),
			meta
		);
		let filter = meta.context.syncback_filter();
		trace!("update_non_project_properties: Filter: {:?}", filter);

		if filter.matches_path(path) {
			filter_warn!(instance.referent(), path);
			trace!("update_non_project_properties: Exiting function early (path filtered)");
			return Ok(());
		}

		let mut properties = validate_properties(properties, filter);
		trace!("update_non_project_properties: Validated properties: {:?}", properties);

		if let Some(middleware) = Middleware::from_class(
			&instance.class,
			if !meta.context.use_legacy_scripts() {
				trace!("update_non_project_properties: Using modern script handling");
				Some(&mut properties)
			} else {
				trace!("update_non_project_properties: Using legacy script handling");
				None
			},
		) {
			trace!("update_non_project_properties: Found middleware: {:?}", middleware);
			let new_path = meta
				.context
				.sync_rules_of_type(&middleware, true)
				.iter()
				.find_map(|rule| {
					trace!("update_non_project_properties: Checking file sync rule: {:?}", rule);
					let located = rule.locate(path, &instance.name, vfs.is_dir(path));
					trace!("update_non_project_properties: Rule locate result: {:?}", located);
					located
				});
			trace!(
				"update_non_project_properties: Located potential new file path: {:?}",
				new_path
			);

			let file_path = if let Some(SourceEntry::File(path_entry)) = meta.source.get_file_mut() {
				let mut current_path = path_entry.to_owned();
				trace!(
					"update_non_project_properties: Found existing file path in meta: {}",
					current_path.display()
				);

				if let Some(new_path) = new_path {
					if current_path != new_path {
						trace!(
							"update_non_project_properties: Renaming file path from {} to {}",
							current_path.display(),
							new_path.display()
						);
						vfs.rename(&current_path, &new_path)?;

						*path_entry = new_path.clone();
						current_path = new_path;
					} else {
						trace!("update_non_project_properties: New path is same as current path, no rename needed.");
					}
				}

				Some(current_path)
			} else {
				trace!("update_non_project_properties: No existing file path in meta.");
				if let Some(new_path) = &new_path {
					trace!(
						"update_non_project_properties: Adding located path {} to meta",
						new_path.display()
					);
					meta.source.add_file(new_path);
				}

				new_path
			};

			trace!(
				"update_non_project_properties: Final file_path for writing: {:?}",
				file_path
			);

			if let Some(file_path) = file_path {
				trace!(
					"update_non_project_properties: Writing properties using middleware to {}",
					file_path.display()
				);
				let properties = middleware.write(properties.clone(), &file_path, vfs)?;
				trace!(
					"update_non_project_properties: Middleware write complete, remaining properties: {:?}",
					properties
				);

				if let Some(data_path) = locate_instance_data(&instance.name, path, meta, vfs) {
					trace!(
						"update_non_project_properties: Located data path: {}",
						data_path.display()
					);
					if filter.matches_path(&data_path) {
						filter_warn!(instance.referent(), &data_path);
						trace!("update_non_project_properties: Data path filtered, skipping data write.");
					} else {
						trace!("update_non_project_properties: Writing data to {}", data_path.display());
						let data_path =
							data::write_data(true, &instance.class, properties.clone(), &data_path, meta, vfs)?;
						trace!("update_non_project_properties: Wrote data to path: {:?}", data_path);
						meta.source.set_data(data_path);
						trace!(
							"update_non_project_properties: Updated meta source with data path: {:?}",
							meta.source
						);
					}
				} else {
					trace!("update_non_project_properties: Could not locate data path, skipping data write.");
				}
			} else {
				error!(
					"update_non_project_properties: Failed to locate file for path {:?}",
					path.display()
				);
			}
		} else if let Some(data_path) = locate_instance_data(&instance.name, path, meta, vfs) {
			trace!(
				"update_non_project_properties: No middleware found, but located data path: {}",
				data_path.display()
			);
			if filter.matches_path(&data_path) {
				filter_warn!(instance.referent(), &data_path);
				trace!("update_non_project_properties: Data path filtered, skipping data write.");
			} else {
				trace!("update_non_project_properties: Writing data to {}", data_path.display());
				let data_path = data::write_data(false, &instance.class, properties.clone(), &data_path, meta, vfs)?;
				trace!("update_non_project_properties: Wrote data to path: {:?}", data_path);
				meta.source.set_data(data_path);
				trace!(
					"update_non_project_properties: Updated meta source with data path: {:?}",
					meta.source
				);
			}
		} else {
			trace!(
				"update_non_project_properties: No middleware and could not locate data path, skipping property write."
			);
		}

		instance.properties = properties;
		trace!(
			"update_non_project_properties: Updated instance properties in tree: {:?}",
			instance.properties
		);

		trace!("update_non_project_properties: Exiting function successfully");
		Ok(())
	}

	trace!("apply_update: Matching source kind: {:?}", meta.source.get());
	match meta.source.get().clone() {
		SourceKind::Path(mut path) => {
			trace!("apply_update: Source is Path: {}", path.display());
			if let Some(mut name) = snapshot.name {
				trace!("apply_update: Handling name update to: {}", name);
				let original_name = meta.original_name.clone();
				trace!("apply_update: Original name from meta: {:?}", original_name);

				if !verify_name(&mut name, &mut meta) {
					trace!("apply_update: Name verification failed for {}, exiting early.", name);
					return Ok(());
				}
				trace!("apply_update: Verified name: {}, updated meta: {:?}", name, meta);

				path = rename_path(&path, &instance.name, &name);
				trace!("apply_update: Calculated new path based on rename: {}", path.display());

				if !verify_path(&mut path, &mut name, &mut meta, vfs) {
					trace!(
						"apply_update: Path verification failed for {}, exiting early.",
						path.display()
					);
					return Ok(());
				}
				trace!(
					"apply_update: Verified path: {}, updated name: {}, updated meta: {:?}",
					path.display(),
					name,
					meta
				);

				*meta.source.get_mut() = SourceKind::Path(path.clone());
				trace!("apply_update: Updated source kind in meta: {:?}", meta.source.get());

				let filter = meta.context.syncback_filter();
				trace!("apply_update: Filter for renaming relevant paths: {:?}", filter);

				if let Some(SourceEntry::Folder(folder_path_entry)) = meta.source.get_folder_mut() {
					let current_folder_path = folder_path_entry.to_owned();
					trace!(
						"apply_update: Renaming folder source from {}",
						current_folder_path.display()
					);
					let new_path = current_folder_path.with_file_name(&name);
					trace!("apply_update: New folder path: {}", new_path.display());

					if filter.matches_path(&current_folder_path) && filter.matches_path(&new_path) {
						filter_warn!(snapshot.id, &current_folder_path);
						trace!("apply_update: Both old and new folder paths filtered, skipping rename.");
					} else {
						trace!(
							"apply_update: Performing VFS rename for folder: {} -> {}",
							current_folder_path.display(),
							new_path.display()
						);
						vfs.rename(&current_folder_path, &new_path)?;
						*folder_path_entry = new_path.clone();
						trace!("apply_update: Updated folder path in meta source.");

						for entry in meta.source.relevant_mut() {
							trace!("apply_update: Updating relevant path entry: {:?}", entry);
							match entry {
								SourceEntry::File(path_entry) | SourceEntry::Data(path_entry) => {
									let original_relevant_path = path_entry.clone();
									*path_entry = new_path.join(path_entry.get_name());
									trace!(
										"apply_update: Updated relevant path from {} to {}",
										original_relevant_path.display(),
										path_entry.display()
									);
								}
								_ => {
									trace!("apply_update: Skipping non-file/data relevant entry.");
									continue;
								}
							}
						}
					}
				} else {
					trace!("apply_update: Renaming relevant file/data sources (not folder source)");
					for entry in meta.source.relevant_mut() {
						trace!("apply_update: Updating relevant path entry: {:?}", entry);
						match entry {
							SourceEntry::File(path_entry) | SourceEntry::Data(path_entry) => {
								let current_path = path_entry.clone();
								let new_path = rename_path(&current_path, &instance.name, &name);
								trace!("apply_update: Calculated new relevant path: {}", new_path.display());

								if filter.matches_path(&current_path) && filter.matches_path(&new_path) {
									filter_warn!(snapshot.id, &current_path);
									trace!("apply_update: Both old and new relevant paths filtered, skipping rename.");
									continue;
								}

								trace!(
									"apply_update: Performing VFS rename for relevant path: {} -> {}",
									current_path.display(),
									new_path.display()
								);
								vfs.rename(&current_path, &new_path)?;
								*path_entry = new_path;
								trace!("apply_update: Updated relevant path in meta source.");
							}
							_ => {
								trace!("apply_update: Skipping non-file/data relevant entry.");
								continue;
							}
						}
					}
				}

				if original_name != meta.original_name && snapshot.properties.is_none() {
					trace!("apply_update: Name changed and no properties updated, attempting to write original name metadata.");
					if let Some(data_path) = locate_instance_data(&name, &path, &meta, vfs) {
						trace!(
							"apply_update: Located data path for original name metadata: {}",
							data_path.display()
						);
						if filter.matches_path(&data_path) {
							filter_warn!(instance.referent(), &data_path);
							trace!("apply_update: Data path filtered, skipping original name write.");
						} else {
							trace!("apply_update: Writing original name to {}", data_path.display());
							write_original_name(&data_path, &meta, vfs)?;
						}
					} else {
						trace!("apply_update: Could not locate data path for original name metadata.");
					}
				}

				instance.name = meta.original_name.clone().unwrap_or(name);
				trace!("apply_update: Updated instance name in tree to: {}", instance.name);
			}

			if let Some(properties) = snapshot.properties {
				trace!("apply_update: Handling property update: {:?}", properties);
				update_non_project_properties(&path, properties, instance, &mut meta, vfs)?;
			} else {
				trace!("apply_update: No properties to update.");
			}

			tree.update_meta(snapshot.id, meta);
			trace!("apply_update: Updated meta in tree for instance {:?}", snapshot.id);

			if let Some(class) = snapshot.class {
				// You can't change the class of an instance inside Roblox Studio
				warn!(
					"apply_update: Received unexpected class update for {:?} to {}, ignoring.",
					snapshot.id, class
				);
				unreachable!()
			}

			if let Some(meta_update) = snapshot.meta {
				// Currently Argon client does not update meta
				warn!(
					"apply_update: Received unexpected meta update for {:?}: {:?}, ignoring.",
					snapshot.id, meta_update
				);
				unreachable!()
			}
		}
		SourceKind::Project(name, path, node, node_path) => {
			trace!(
				"apply_update: Source is Project: name={}, path={}, node_path={:?}",
				name,
				path.display(),
				node_path
			);
			let mut project = Project::load(&path)?;
			trace!("apply_update: Loaded project from {}", path.display());

			if let Some(properties) = snapshot.properties {
				trace!(
					"apply_update: Handling property update for project node: {:?}",
					properties
				);
				if let Some(custom_path) = node.path {
					trace!("apply_update: Project node has custom path: {:?}", custom_path);
					let custom_path = path.with_file_name(custom_path.path()).clean();
					trace!("apply_update: Resolved custom path: {}", custom_path.display());

					update_non_project_properties(&custom_path, properties, instance, &mut meta, vfs)?;
					trace!("apply_update: Updated properties via non-project logic due to custom path.");

					let node = project
						.find_node_by_path(&node_path)
						.context(format!("Failed to find project node with path {:?}", node_path))?;
					trace!("apply_update: Found project node: {:?}", node);

					// Clear project node properties as they are now managed externally
					node.properties = UstrMap::new();
					node.attributes = None;
					node.tags = vec![];
					node.keep_unknowns = None;
					trace!("apply_update: Cleared properties/attributes/tags on project node.");
				} else {
					trace!("apply_update: Project node does not have custom path, updating node directly.");
					let node = project
						.find_node_by_path(&node_path)
						.context(format!("Failed to find project node with path {:?}", node_path))?;
					trace!("apply_update: Found project node: {:?}", node);

					let class = node.class_name.unwrap_or_else(|| Ustr::from(&name));
					trace!("apply_update: Determined class for property serialization: {}", class);
					let properties = validate_properties(properties, meta.context.syncback_filter());
					trace!("apply_update: Validated properties for project node: {:?}", properties);

					node.properties = serialize_properties(&class, properties.clone());
					trace!(
						"apply_update: Serialized and set properties on project node: {:?}",
						node.properties
					);
					node.tags = vec![]; // TODO: Handle tags if necessary
					node.keep_unknowns = None; // TODO: Handle keep_unknowns

					instance.properties = properties;
					trace!(
						"apply_update: Updated instance properties in tree: {:?}",
						instance.properties
					);
				}
			} else {
				trace!("apply_update: No properties to update for project node.");
			}

			// It has to be done after updating properties as it may change the node path
			if let Some(new_name) = snapshot.name {
				trace!("apply_update: Handling name update for project node to: {}", new_name);
				let parent_node_path = node_path.parent();
				trace!(
					"apply_update: Finding parent project node at path: {:?}",
					parent_node_path
				);
				let parent_node = project
					.find_node_by_path(&parent_node_path)
					.with_context(|| format!("Failed to find parent project node with path {:?}", parent_node_path))?;
				trace!("apply_update: Found parent project node.");

				trace!("apply_update: Removing old node '{}' from parent's tree", name);
				let node = parent_node
					.tree
					.remove(&name)
					.context(format!("Failed to remove project node with path {:?}", node_path))?;
				trace!("apply_update: Removed node: {:?}", node);

				trace!(
					"apply_update: Inserting node with new name '{}' into parent's tree",
					new_name
				);
				parent_node.tree.insert(new_name.clone(), node.clone());

				let new_node_path = parent_node_path.join(&new_name);
				trace!("apply_update: New node path: {:?}", new_node_path);

				*meta.source.get_mut() =
					SourceKind::Project(new_name.clone(), path.clone(), Box::new(node), new_node_path);
				trace!("apply_update: Updated source kind in meta: {:?}", meta.source.get());

				instance.name = new_name;
				trace!("apply_update: Updated instance name in tree to: {}", instance.name);
			} else {
				trace!("apply_update: No name update for project node.");
			}

			tree.update_meta(snapshot.id, meta);
			trace!("apply_update: Updated meta in tree for instance {:?}", snapshot.id);
			project.save(&path)?;
			trace!("apply_update: Saved project to {}", path.display());

			if let Some(class) = snapshot.class {
				// You can't change the class of an instance inside Roblox Studio
				warn!(
					"apply_update: Received unexpected class update for project node {:?} to {}, ignoring.",
					snapshot.id, class
				);
				unreachable!()
			}

			if let Some(meta_update) = snapshot.meta {
				// Currently Argon client does not update meta
				warn!(
					"apply_update: Received unexpected meta update for project node {:?}: {:?}, ignoring.",
					snapshot.id, meta_update
				);
				unreachable!()
			}
		}
		SourceKind::None => {
			let msg = format!(
				"apply_update: Attempted to update instance {:?} with no source",
				snapshot.id
			);
			error!("{}", msg);
			panic!("{}", msg);
		}
	}

	trace!("apply_update: Exiting function successfully");
	Ok(())
}

pub fn apply_removal(id: Ref, tree: &mut Tree, vfs: &Vfs) -> Result<()> {
	trace!("Removing {:?}", id);

	if let Some(instance) = tree.get_instance(id) {
		let filter = tree.get_meta(id).unwrap().context.syncback_filter();
		trace!("apply_removal: Instance {:?} exists. Filter: {:?}", id, filter);

		if filter.matches_name(&instance.name) || filter.matches_class(&instance.class) {
			filter_warn!(id);
			trace!("apply_removal: Exiting function early (instance filtered)");
			return Ok(());
		}
	} else {
		warn!(
			"apply_removal: Attempted to remove instance that doesn't exist: {:?}",
			id
		);
		trace!("apply_removal: Exiting function early (instance does not exist)");
		return Ok(());
	}

	let meta = tree.get_meta(id).unwrap().clone();
	trace!("apply_removal: Got meta for instance {:?}: {:?}", id, meta);

	fn remove_non_project_instances(id: Ref, meta: &Meta, tree: &mut Tree, vfs: &Vfs) -> Result<()> {
		trace!(
			"remove_non_project_instances: Entering function with id={:?}, meta={:?}",
			id,
			meta
		);
		let filter = meta.context.syncback_filter();
		trace!("remove_non_project_instances: Filter: {:?}", filter);

		for entry in meta.source.relevant() {
			trace!(
				"remove_non_project_instances: Processing relevant source entry: {:?}",
				entry
			);
			match entry {
				SourceEntry::Project(_) => {
					trace!("remove_non_project_instances: Skipping project entry.");
					continue;
				}
				_ => {
					let path = entry.path();
					trace!("remove_non_project_instances: Processing path: {}", path.display());

					if vfs.exists(path) {
						trace!("remove_non_project_instances: Path exists.");
						if filter.matches_path(path) {
							filter_warn!(id, path);
							trace!("remove_non_project_instances: Path filtered, skipping removal.");
						} else {
							trace!("remove_non_project_instances: Removing path via VFS.");
							vfs.remove(path)?
						}
					} else {
						trace!("remove_non_project_instances: Path does not exist, skipping removal.");
					}
				}
			}
		}

		// Transform parent instance source from folder to file
		// if it no longer has any children
		trace!("remove_non_project_instances: Checking if parent needs transformation from folder to file");

		let parent = tree
			.get_instance(id)
			.and_then(|instance| {
				let parent_id = instance.parent();
				trace!(
					"remove_non_project_instances: Instance {:?} parent ID: {:?}",
					id,
					parent_id
				);
				tree.get_instance(parent_id)
			})
			.context("Instance has no parent or parent does not exist in tree")?;
		trace!(
			"remove_non_project_instances: Found parent instance: {:?}",
			parent.referent()
		);

		if parent.children().len() != 1 {
			trace!(
				"remove_non_project_instances: Parent {:?} has {} children (expected 1 after removal), skipping transformation.",
				parent.referent(), parent.children().len()
			);
			return Ok(());
		}

		trace!(
			"remove_non_project_instances: Parent {:?} has only 1 child remaining, proceeding with potential transformation.",
			parent.referent()
		);
		let parent_ref = parent.referent();
		let meta = tree.get_meta_mut(parent_ref).unwrap();
		trace!("remove_non_project_instances: Got mutable meta for parent: {:?}", meta);

		if let SourceKind::Path(folder_path) = meta.source.get().clone() {
			trace!(
				"remove_non_project_instances: Parent source is Path (potential folder): {}",
				folder_path.display()
			);
			let name = folder_path.get_name();
			trace!("remove_non_project_instances: Parent folder name: {}", name);

			if let Some(file_entry) = meta.source.get_file().cloned() {
				let file_path_in_folder = file_entry.path();
				trace!(
					"remove_non_project_instances: Parent meta has associated file: {}",
					file_path_in_folder.display()
				);
				let file_path_outside_folder =
					meta.context
						.sync_rules()
						.iter()
						.find(|rule| {
							let matches = rule.matches_child(file_path_in_folder);
							trace!(
							"remove_non_project_instances: Checking sync rule {:?} against child file {}: matches={}",
							rule, file_path_in_folder.display(), matches
						);
							matches
						})
						.and_then(|rule| {
							let located = rule.locate(&folder_path, name, false);
							trace!(
								"remove_non_project_instances: Located potential new path using rule {:?}: {:?}",
								rule,
								located
							);
							located
						});

				if let Some(new_path) = file_path_outside_folder {
					trace!(
						"remove_non_project_instances: Located new path for file: {}",
						new_path.display()
					);
					vfs.rename(file_path_in_folder, &new_path)?;
					trace!(
						"remove_non_project_instances: Renamed file {} to {}",
						file_path_in_folder.display(),
						new_path.display()
					);
					let mut source = Source::file(&new_path);
					trace!("remove_non_project_instances: Created new file source: {:?}", source);

					if let Some(data_entry) = meta.source.get_data().cloned() {
						let data_path_in_folder = data_entry.path();
						trace!(
							"remove_non_project_instances: Parent meta has associated data: {}",
							data_path_in_folder.display()
						);
						let data_path_outside_folder = meta
							.context
							.sync_rules_of_type(&Middleware::InstanceData, true)
							.iter()
							.find_map(|rule| {
								let located = rule.locate(&folder_path, name, false);
								trace!(
									"remove_non_project_instances: Checking data sync rule {:?} for potential new path: {:?}",
									rule, located
								);
								located
							});

						if let Some(new_data_path) = data_path_outside_folder {
							trace!(
								"remove_non_project_instances: Located new path for data: {}",
								new_data_path.display()
							);
							vfs.rename(data_path_in_folder, &new_data_path)?;
							trace!(
								"remove_non_project_instances: Renamed data {} to {}",
								data_path_in_folder.display(),
								new_data_path.display()
							);
							source.add_data(&new_data_path);
							trace!(
								"remove_non_project_instances: Added data path to new source: {:?}",
								source
							);
						} else {
							trace!("remove_non_project_instances: Could not locate new path for data.");
						}
					} else {
						trace!("remove_non_project_instances: No data associated with parent meta.");
					}

					vfs.remove(&folder_path)?;
					trace!(
						"remove_non_project_instances: Removed original folder {}",
						folder_path.display()
					);
					meta.set_source(source);
					trace!(
						"remove_non_project_instances: Set parent meta source to new file source: {:?}",
						meta.source
					);
				} else {
					trace!("remove_non_project_instances: Could not locate new path for file, transformation aborted.");
				}
			} else {
				trace!("remove_non_project_instances: Parent meta does not have an associated file entry, cannot transform.");
			}
		} else {
			trace!("remove_non_project_instances: Parent source is not a Path or is not a folder, skipping transformation.");
		}

		trace!("remove_non_project_instances: Exiting function successfully");
		Ok(())
	}

	trace!("apply_removal: Matching source kind: {:?}", meta.source.get());
	match meta.source.get().clone() {
		SourceKind::Path(_) => {
			trace!("apply_removal: Source is Path, calling remove_non_project_instances");
			remove_non_project_instances(id, &meta, tree, vfs)?;
		}
		SourceKind::Project(name, path, node, node_path) => {
			trace!(
				"apply_removal: Source is Project: name={}, path={}, node_path={:?}",
				name,
				path.display(),
				node_path
			);
			let mut project = Project::load(&path)?;
			trace!("apply_removal: Loaded project from {}", path.display());
			let parent_node_path = node_path.parent();
			trace!(
				"apply_removal: Finding parent project node at path: {:?}",
				parent_node_path
			);
			let parent_node = project.find_node_by_path(&parent_node_path);

			trace!("apply_removal: Attempting to remove node '{}' from parent's tree", name);
			let removed_node = parent_node.and_then(|node| node.tree.remove(&name)).ok_or_else(|| {
				let msg = format!(
					"apply_removal: Failed to remove instance {:?} (name: {}) from project node tree at path {:?}",
					id, name, parent_node_path
				);
				error!("{}", msg);
				anyhow!(msg)
			})?;
			trace!(
				"apply_removal: Successfully removed node from project tree: {:?}",
				removed_node
			);

			if node.path.is_some() {
				trace!("apply_removal: Project node had a custom path, calling remove_non_project_instances to clean up external files.");
				remove_non_project_instances(id, &meta, tree, vfs)?;
			} else {
				trace!("apply_removal: Project node did not have a custom path.");
			}

			project.save(&path)?;
			trace!("apply_removal: Saved project to {}", path.display());
		}
		SourceKind::None => {
			let msg = format!("apply_removal: Attempted to remove instance {:?} with no source", id);
			error!("{}", msg);
			panic!("{}", msg);
		}
	}

	tree.remove_instance(id);
	trace!("apply_removal: Removed instance {:?} from tree.", id);

	trace!("apply_removal: Exiting function successfully");
	Ok(())
}

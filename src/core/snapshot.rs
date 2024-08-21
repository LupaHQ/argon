use rbx_dom_weak::types::{Ref, Variant};
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap,
	fmt::{self, Debug, Formatter},
};

use super::meta::Meta;
use crate::{middleware::data::DataSnapshot, Properties};

#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
	pub id: Ref,
	pub meta: Meta,

	// Roblox related
	pub name: String,
	pub class: String,
	pub properties: Properties,
	pub children: Vec<Snapshot>,
}

impl Snapshot {
	// Creating new snapshot

	pub fn new() -> Self {
		Self {
			id: Ref::none(),
			meta: Meta::new(),
			name: String::new(),
			class: String::from("Folder"),
			properties: HashMap::new(),
			children: Vec::new(),
		}
	}

	pub fn with_id(mut self, id: Ref) -> Self {
		self.set_id(id);
		self
	}

	pub fn with_meta(mut self, meta: Meta) -> Self {
		self.set_meta(meta);
		self
	}

	pub fn with_name(mut self, name: &str) -> Self {
		self.set_name(name);
		self
	}

	pub fn with_class(mut self, class: &str) -> Self {
		self.set_class(class);
		self
	}

	pub fn with_properties(mut self, properties: Properties) -> Self {
		self.set_properties(properties);
		self
	}

	pub fn with_children(mut self, children: Vec<Snapshot>) -> Self {
		self.set_children(children);
		self
	}

	pub fn with_data(mut self, data: DataSnapshot) -> Self {
		self.apply_data(data);
		self
	}

	// Overwriting snapshot fields

	pub fn set_id(&mut self, id: Ref) {
		self.id = id;
	}

	pub fn set_meta(&mut self, meta: Meta) {
		self.meta = meta;
	}

	pub fn set_name(&mut self, name: &str) {
		name.clone_into(&mut self.name);
	}

	pub fn set_class(&mut self, class: &str) {
		class.clone_into(&mut self.class);
	}

	pub fn set_properties(&mut self, properties: Properties) {
		self.properties = properties;
	}

	pub fn set_children(&mut self, children: Vec<Snapshot>) {
		self.children = children;
	}

	pub fn apply_data(&mut self, data: DataSnapshot) {
		if let Some(class) = data.class {
			self.class = class;
		}

		if let Some(keep_unknowns) = data.keep_unknowns {
			self.meta.keep_unknowns = keep_unknowns;
		}

		if let Some(mesh_source) = data.mesh_source {
			self.meta.set_mesh_source(Some(mesh_source));
		}

		self.extend_properties(data.properties);
		self.meta.source.add_data(&data.path);
	}

	// Adding to snapshot fields

	pub fn add_property(&mut self, name: &str, value: Variant) {
		self.properties.insert(name.to_owned(), value);
	}

	pub fn add_child(&mut self, child: Snapshot) {
		self.children.push(child);
	}

	// Joining snapshot fields

	pub fn extend_properties(&mut self, properties: Properties) {
		self.properties.extend(properties);
	}

	pub fn extend_children(&mut self, children: Vec<Snapshot>) {
		self.children.extend(children);
	}
}

impl Debug for Snapshot {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		let mut debug = f.debug_struct("Snapshot");

		debug.field("name", &self.name);
		debug.field("class", &self.class);
		debug.field("id", &self.id);
		debug.field("meta", &self.meta);

		if !self.properties.is_empty() {
			let mut properties = self.properties.clone();

			if let Some(property) = properties.get_mut("Source") {
				if let Variant::String(source) = property {
					let lines = source.lines().count();

					if lines > 1 {
						*property = Variant::String(format!("Truncated... ({} lines)", lines));
					}
				}
			}

			debug.field("properties", &properties);
		}

		if !self.children.is_empty() {
			debug.field("children", &self.children);
		}

		debug.finish()
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedSnapshot {
	pub id: Ref,
	pub meta: Meta,
	pub parent: Ref,
	pub name: String,
	pub class: String,
	pub properties: Properties,
	pub children: Vec<Snapshot>,
}

impl AddedSnapshot {
	pub fn to_snapshot(self) -> Snapshot {
		Snapshot {
			id: self.id,
			meta: self.meta,
			name: self.name,
			class: self.class,
			properties: self.properties,
			children: self.children,
		}
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatedSnapshot {
	pub id: Ref,
	pub meta: Option<Meta>,
	pub name: Option<String>,
	pub class: Option<String>,
	pub properties: Option<Properties>,
}

impl UpdatedSnapshot {
	pub fn new(id: Ref) -> Self {
		Self {
			id,
			name: None,
			class: None,
			properties: None,
			meta: None,
		}
	}

	pub fn is_empty(&self) -> bool {
		self.name.is_none() && self.class.is_none() && self.properties.is_none()
	}
}

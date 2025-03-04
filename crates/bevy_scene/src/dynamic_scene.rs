use crate::{ron, DynamicSceneBuilder, Scene, SceneSpawnError};
use bevy_ecs::{
    entity::Entity,
    reflect::{AppTypeRegistry, ReflectComponent, ReflectMapEntities},
    world::World,
};
use bevy_reflect::{Reflect, TypePath, TypeRegistryArc};
use bevy_utils::{EntityHashMap, HashMap};
use std::any::TypeId;

#[cfg(feature = "serialize")]
use crate::serde::SceneSerializer;
use bevy_asset::Asset;
use bevy_ecs::reflect::ReflectResource;
#[cfg(feature = "serialize")]
use serde::Serialize;

/// A collection of serializable resources and dynamic entities.
///
/// Each dynamic entity in the collection contains its own run-time defined set of components.
/// To spawn a dynamic scene, you can use either:
/// * [`SceneSpawner::spawn_dynamic`](crate::SceneSpawner::spawn_dynamic)
/// * adding the [`DynamicSceneBundle`](crate::DynamicSceneBundle) to an entity
/// * adding the [`Handle<DynamicScene>`](bevy_asset::Handle) to an entity (the scene will only be
/// visible if the entity already has [`Transform`](bevy_transform::components::Transform) and
/// [`GlobalTransform`](bevy_transform::components::GlobalTransform) components)
/// * using the [`DynamicSceneBuilder`] to construct a `DynamicScene` from `World`.
#[derive(Asset, TypePath, Default)]
pub struct DynamicScene {
    /// Resources stored in the dynamic scene.
    pub resources: Vec<Box<dyn Reflect>>,
    /// Entities contained in the dynamic scene.
    pub entities: Vec<DynamicEntity>,
}

/// A reflection-powered serializable representation of an entity and its components.
pub struct DynamicEntity {
    /// The identifier of the entity, unique within a scene (and the world it may have been generated from).
    ///
    /// Components that reference this entity must consistently use this identifier.
    pub entity: Entity,
    /// A vector of boxed components that belong to the given entity and
    /// implement the [`Reflect`] trait.
    pub components: Vec<Box<dyn Reflect>>,
}

impl DynamicScene {
    /// Create a new dynamic scene from a given scene.
    pub fn from_scene(scene: &Scene) -> Self {
        Self::from_world(&scene.world)
    }

    /// Create a new dynamic scene from a given world.
    pub fn from_world(world: &World) -> Self {
        DynamicSceneBuilder::from_world(world)
            .extract_entities(world.iter_entities().map(|entity| entity.id()))
            .extract_resources()
            .build()
    }

    /// Write the resources, the dynamic entities, and their corresponding components to the given world.
    ///
    /// This method will return a [`SceneSpawnError`] if a type either is not registered
    /// in the provided [`AppTypeRegistry`] resource, or doesn't reflect the
    /// [`Component`](bevy_ecs::component::Component) or [`Resource`](bevy_ecs::prelude::Resource) trait.
    pub fn write_to_world_with(
        &self,
        world: &mut World,
        entity_map: &mut EntityHashMap<Entity, Entity>,
        type_registry: &AppTypeRegistry,
    ) -> Result<(), SceneSpawnError> {
        let type_registry = type_registry.read();

        for resource in &self.resources {
            let type_info = resource.get_represented_type_info().ok_or_else(|| {
                SceneSpawnError::NoRepresentedType {
                    type_path: resource.reflect_type_path().to_string(),
                }
            })?;
            let registration = type_registry.get(type_info.type_id()).ok_or_else(|| {
                SceneSpawnError::UnregisteredButReflectedType {
                    type_path: type_info.type_path().to_string(),
                }
            })?;
            let reflect_resource = registration.data::<ReflectResource>().ok_or_else(|| {
                SceneSpawnError::UnregisteredResource {
                    type_path: type_info.type_path().to_string(),
                }
            })?;

            // If the world already contains an instance of the given resource
            // just apply the (possibly) new value, otherwise insert the resource
            reflect_resource.apply_or_insert(world, &**resource);
        }

        // For each component types that reference other entities, we keep track
        // of which entities in the scene use that component.
        // This is so we can update the scene-internal references to references
        // of the actual entities in the world.
        let mut scene_mappings: HashMap<TypeId, Vec<Entity>> = HashMap::default();

        for scene_entity in &self.entities {
            // Fetch the entity with the given entity id from the `entity_map`
            // or spawn a new entity with a transiently unique id if there is
            // no corresponding entry.
            let entity = *entity_map
                .entry(scene_entity.entity)
                .or_insert_with(|| world.spawn_empty().id());
            let entity_mut = &mut world.entity_mut(entity);

            // Apply/ add each component to the given entity.
            for component in &scene_entity.components {
                let type_info = component.get_represented_type_info().ok_or_else(|| {
                    SceneSpawnError::NoRepresentedType {
                        type_path: component.reflect_type_path().to_string(),
                    }
                })?;
                let registration = type_registry.get(type_info.type_id()).ok_or_else(|| {
                    SceneSpawnError::UnregisteredButReflectedType {
                        type_path: type_info.type_path().to_string(),
                    }
                })?;
                let reflect_component =
                    registration.data::<ReflectComponent>().ok_or_else(|| {
                        SceneSpawnError::UnregisteredComponent {
                            type_path: type_info.type_path().to_string(),
                        }
                    })?;

                // If this component references entities in the scene, track it
                // so we can update it to the entity in the world.
                if registration.data::<ReflectMapEntities>().is_some() {
                    scene_mappings
                        .entry(registration.type_id())
                        .or_insert(Vec::new())
                        .push(entity);
                }

                // If the entity already has the given component attached,
                // just apply the (possibly) new value, otherwise add the
                // component to the entity.
                reflect_component.apply_or_insert(entity_mut, &**component, &type_registry);
            }
        }

        // Updates references to entities in the scene to entities in the world
        for (type_id, entities) in scene_mappings.into_iter() {
            let registration = type_registry.get(type_id).expect(
                "we should be getting TypeId from this TypeRegistration in the first place",
            );
            if let Some(map_entities_reflect) = registration.data::<ReflectMapEntities>() {
                map_entities_reflect.map_entities(world, entity_map, &entities);
            }
        }

        Ok(())
    }

    /// Write the resources, the dynamic entities, and their corresponding components to the given world.
    ///
    /// This method will return a [`SceneSpawnError`] if a type either is not registered
    /// in the world's [`AppTypeRegistry`] resource, or doesn't reflect the
    /// [`Component`](bevy_ecs::component::Component) trait.
    pub fn write_to_world(
        &self,
        world: &mut World,
        entity_map: &mut EntityHashMap<Entity, Entity>,
    ) -> Result<(), SceneSpawnError> {
        let registry = world.resource::<AppTypeRegistry>().clone();
        self.write_to_world_with(world, entity_map, &registry)
    }

    // TODO: move to AssetSaver when it is implemented
    /// Serialize this dynamic scene into rust object notation (ron).
    #[cfg(feature = "serialize")]
    pub fn serialize_ron(&self, registry: &TypeRegistryArc) -> Result<String, ron::Error> {
        serialize_ron(SceneSerializer::new(self, registry))
    }
}

/// Serialize a given Rust data structure into rust object notation (ron).
#[cfg(feature = "serialize")]
pub fn serialize_ron<S>(serialize: S) -> Result<String, ron::Error>
where
    S: Serialize,
{
    let pretty_config = ron::ser::PrettyConfig::default()
        .indentor("  ".to_string())
        .new_line("\n".to_string());
    ron::ser::to_string_pretty(&serialize, pretty_config)
}

#[cfg(test)]
mod tests {
    use bevy_ecs::{reflect::AppTypeRegistry, system::Command, world::World};
    use bevy_hierarchy::{Parent, PushChild};
    use bevy_utils::EntityHashMap;

    use crate::dynamic_scene_builder::DynamicSceneBuilder;

    #[test]
    fn components_not_defined_in_scene_should_not_be_affected_by_scene_entity_map() {
        // Testing that scene reloading applies EntityMap correctly to MapEntities components.

        // First, we create a simple world with a parent and a child relationship
        let mut world = World::new();
        world.init_resource::<AppTypeRegistry>();
        world
            .resource_mut::<AppTypeRegistry>()
            .write()
            .register::<Parent>();
        let original_parent_entity = world.spawn_empty().id();
        let original_child_entity = world.spawn_empty().id();
        PushChild {
            parent: original_parent_entity,
            child: original_child_entity,
        }
        .apply(&mut world);

        // We then write this relationship to a new scene, and then write that scene back to the
        // world to create another parent and child relationship
        let scene = DynamicSceneBuilder::from_world(&world)
            .extract_entity(original_parent_entity)
            .extract_entity(original_child_entity)
            .build();
        let mut entity_map = EntityHashMap::default();
        scene.write_to_world(&mut world, &mut entity_map).unwrap();

        let &from_scene_parent_entity = entity_map.get(&original_parent_entity).unwrap();
        let &from_scene_child_entity = entity_map.get(&original_child_entity).unwrap();

        // We then add the parent from the scene as a child of the original child
        // Hierarchy should look like:
        // Original Parent <- Original Child <- Scene Parent <- Scene Child
        PushChild {
            parent: original_child_entity,
            child: from_scene_parent_entity,
        }
        .apply(&mut world);

        // We then reload the scene to make sure that from_scene_parent_entity's parent component
        // isn't updated with the entity map, since this component isn't defined in the scene.
        // With bevy_hierarchy, this can cause serious errors and malformed hierarchies.
        scene.write_to_world(&mut world, &mut entity_map).unwrap();

        assert_eq!(
            original_parent_entity,
            world
                .get_entity(original_child_entity)
                .unwrap()
                .get::<Parent>()
                .unwrap()
                .get(),
            "something about reloading the scene is touching entities with the same scene Ids"
        );
        assert_eq!(
            original_child_entity,
            world
                .get_entity(from_scene_parent_entity)
                .unwrap()
                .get::<Parent>()
                .unwrap()
                .get(),
            "something about reloading the scene is touching components not defined in the scene but on entities defined in the scene"
        );
        assert_eq!(
            from_scene_parent_entity,
            world
                .get_entity(from_scene_child_entity)
                .unwrap()
                .get::<Parent>()
                .expect("something is wrong with this test, and the scene components don't have a parent/child relationship")
                .get(),
            "something is wrong with the this test or the code reloading scenes since the relationship between scene entities is broken"
        );
    }
}

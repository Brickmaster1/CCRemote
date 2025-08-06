use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::{
    cell::RefCell,
    fs,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use crate::UiTrait;
use crate::factory::{Factory, FactoryConfig};
use crate::{access::*, config_util::*, process::*, recipe::*, storage::*};
use crate::{detail_cache::DetailCache, server::Server, Tui};
use crate::item::{Filter, DetailStack};

#[derive(Deserialize)]
pub struct DynamicFactoryConfig {
    pub server_port: u16,
    pub min_cycle_time_secs: u64,
    pub log_clients: Vec<String>,
    pub bus_accesses: Vec<BusAccessConfig>,
    pub fluid_bus_accesses: Vec<FluidBusConfig>,
    pub fluid_bus_capacity: i32,
    pub storages: Vec<StorageConfig>,
    pub processes: Vec<ProcessConfig>,
    pub backups: Vec<BusAccessConfig>,
    pub fluid_backups: Vec<FluidBusConfig>,
}

#[derive(Deserialize)]
pub struct BusAccessConfig {
    pub client: String,
    pub addr: String,
}

#[derive(Deserialize)]
pub struct FluidBusConfig {
    pub client: String,
    pub fluid_bus_addrs: Vec<String>,
    pub tank_addr: String,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum StorageConfig {
    Chest {
        accesses: Vec<BusAccessConfig>,
        override_max_stack_size: Option<i32>,
    },
    Drawer {
        accesses: Vec<BusAccessConfig>,
        filters: Vec<ItemFilter>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum ItemFilter {
    Label { value: String },
    Name { value: String },
    Both { label: String, name: String },
    Custom { desc: String },
}

#[derive(Deserialize)]
pub struct SlottedInput {
    pub item: ItemFilter,
    pub slots: Vec<SlotConfig>,
    pub allow_backup: bool,
    pub extra_backup: i32,
}

#[derive(Deserialize)]
pub struct SlotConfig {
    pub slot: usize,
    pub size: i32,
}

#[derive(Deserialize)]
pub struct CraftingRecipe {
    pub outputs: Vec<ItemFilter>,
    pub inputs: Vec<SlottedInput>,
    pub max_sets: i32,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum ProcessConfig {
    ManualUI {
        accesses: Vec<BusAccessConfig>,
    },
    Workbench {
        name: String,
        accesses: Vec<BusAccessConfig>,
        recipes: Vec<CraftingRecipe>,
    },
    Slotted {
        name: String,
        accesses: Vec<BusAccessConfig>,
        input_slots: Vec<usize>,
        extract_filter: Option<String>,
        recipes: Vec<CraftingRecipe>,
        strict_priority: bool,
    },
    Turtle {
        name: String,
        file_name: String,
        client: String,
    },
    RedstoneEmitter {
        accesses: Vec<BusAccessConfig>,
        output_rules: Vec<RedstoneRule>,
    },
}

#[derive(Deserialize)]
pub struct RedstoneRule {
    pub name: String,
    pub off_signal: u8,
    pub on_signal: u8,
    pub trigger_items: Vec<ItemFilter>,
}

impl ItemFilter {
    fn to_filter(&self) -> Filter {
        match self {
            ItemFilter::Label { value } => Filter::Label(s(value)),
            ItemFilter::Name { value } => Filter::Name(s(value)),
            ItemFilter::Both { label, name } => Filter::Both {
                label: s(label),
                name: s(name),
            },
            ItemFilter::Custom { desc } => Filter::Custom {
                desc: s(desc),
                func: Rc::new(|_, _| true),
            },
        }
    }

    fn to_outputs(&self) -> Rc<dyn Outputs> {
        Output::new(self.to_filter(), 1)
    }
}

pub fn build_factory_from_json(ui: Arc<dyn UiTrait>, config_path: &str) -> Arc<Mutex<Factory>> {
    let config = load_dynamic_config(config_path);
    let factory_config = FactoryConfig {
        server_port: config.server_port,
        min_cycle_time: Duration::from_secs(config.min_cycle_time_secs),
        log_clients: config.log_clients.into_iter().map(LocalStr::from).collect(),
        bus_accesses: config.bus_accesses.into_iter().map(|x| BusAccess {
            client: x.client.into(),
            inv_addr: x.addr.clone().into(),
            bus_addr: x.addr.into(),
        }).collect(),
        fluid_bus_accesses: config.fluid_bus_accesses.into_iter().map(|x| FluidAccess {
            client: x.client.into(),
            fluid_bus_addrs: Vec::new(),
            tank_addr: LocalStr::new(),
        }).collect(),
        fluid_bus_capacity: config.fluid_bus_capacity,
        storages: config.storages.into_iter().map(convert_storage).collect(),
        processes: config.processes.into_iter().map(convert_process).collect(),
        backups: config.backups.into_iter().map(|x| BusAccess {
            client: x.client.into(),
            inv_addr: x.addr.clone().into(),
            bus_addr: x.addr.into(),
        }).collect(),
        fluid_backups: config.fluid_backups.into_iter().map(|x| FluidAccess {
            client: x.client.into(),
            fluid_bus_addrs: Vec::new(),
            tank_addr: LocalStr::new(),
        }).collect(),
        ui,
    };

    let factory = factory_config.build(|factory| {
        // Initialize any factory state if needed
    });

    Arc::new(Mutex::new(factory))
}

fn convert_recipe(recipe: &CraftingRecipe) -> CraftingGridRecipe {
    CraftingGridRecipe {
        outputs: recipe
            .outputs
            .iter()
            .map(|o| o.to_outputs())
            .fold(ignore_outputs(0.0), |a, b| a.and(b)),
        inputs: recipe
            .inputs
            .iter()
            .map(|input| CraftingGridInput {
                item: input.item.to_filter(),
                size: input.slots.iter().map(|s| s.size).sum(),
                slots: input.slots.iter().map(|s| s.slot).collect(),
                allow_backup: input.allow_backup,
                extra_backup: input.extra_backup,
            })
            .collect(),
        max_sets: recipe.max_sets,
        non_consumables: Vec::new(),
    }
}

pub fn load_dynamic_config(path: &str) -> DynamicFactoryConfig {
    let content = fs::read_to_string(path).expect("Failed to read config file");
    serde_json::from_str(&content).expect("Failed to parse config file")
}

pub fn start_factory_hot_reload(
    ui: Arc<dyn UiTrait>,
    config_path: &str,
    factory_ref: Arc<Mutex<Option<Arc<Mutex<Factory>>>>>,
) {
    let config_path = config_path.to_string();
    thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<Result<Event, notify::Error>>();
        let mut watcher: RecommendedWatcher = Watcher::new(tx, notify::Config::default()).expect("Failed to create watcher");
        watcher.watch(Path::new(&config_path), RecursiveMode::NonRecursive).expect("Failed to watch config file");
        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    let new_factory = build_factory_from_json(ui.clone(), &config_path);
                    let mut factory_lock = factory_ref.lock().unwrap();
                    *factory_lock = Some(new_factory);
                    ui.log("Factory configuration reloaded from JSON.".to_string(), 1);
                }
                Ok(Err(e)) => ui.log(format!("Notify error: {:?}", e), 6),
                Err(e) => ui.log(format!("Recv error: {:?}", e), 6),
            }
        }
    });
}
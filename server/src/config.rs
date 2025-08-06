use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::{
    cell::RefCell,
    fs,
    rc::Rc,
    sync::{mpsc::channel, Arc, Mutex},
    thread,
    time::Duration,
};

use crate::factory::{Factory, FactoryConfig};
use crate::{access::*, config_util::*, process::*, recipe::*, storage::*};
use crate::{detail_cache::DetailCache, server::Server, Tui};

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
                func: Rc::new(|_, _| true), // Custom filters need special handling
            },
        }
    }
}

pub fn build_factory_from_json(tui: Rc<Tui>, config_path: &str) -> Rc<RefCell<Factory>> {
    let config = load_dynamic_config(config_path);

    FactoryConfig {
        tui: tui.clone(),
        detail_cache: DetailCache::new(&tui, s("detail_cache.txt")),
        server: Server::new(tui, config.server_port),
        min_cycle_time: Duration::from_secs(config.min_cycle_time_secs),
        log_clients: config.log_clients.iter().map(|c| s(c)).collect(),
        bus_accesses: config
            .bus_accesses
            .iter()
            .map(|a| BasicAccess {
                client: s(&a.client),
                addr: s(&a.addr),
            })
            .collect(),
        fluid_bus_accesses: config
            .fluid_bus_accesses
            .iter()
            .map(|f| FluidAccess {
                client: s(&f.client),
                fluid_bus_addrs: f.fluid_bus_addrs.iter().map(|a| s(a)).collect(),
                tank_addr: s(&f.tank_addr),
            })
            .collect(),
        fluid_bus_capacity: config.fluid_bus_capacity,
        backups: config
            .backups
            .iter()
            .map(|a| BasicAccess {
                client: s(&a.client),
                addr: s(&a.addr),
            })
            .collect(),
        fluid_backups: config
            .fluid_backups
            .iter()
            .map(|f| FluidAccess {
                client: s(&f.client),
                fluid_bus_addrs: f.fluid_bus_addrs.iter().map(|a| s(a)).collect(),
                tank_addr: s(&f.tank_addr),
            })
            .collect(),
    }
    .build(|factory| {
        // Add storages
        for storage in &config.storages {
            match storage {
                StorageConfig::Chest {
                    accesses,
                    override_max_stack_size,
                } => {
                    factory.add_storage(ChestConfig {
                        accesses: accesses
                            .iter()
                            .map(|a| BusAccess {
                                client: s(&a.client),
                                inv_addr: s(&a.addr),
                                bus_addr: s(&a.addr),
                            })
                            .collect(),
                        override_max_stack_size: override_max_stack_size.map(|size| {
                            Box::new(move |_| size) as Box<dyn Fn(i32) -> i32>
                        }),
                    });
                }
                StorageConfig::Drawer { accesses, filters } => {
                    factory.add_storage(DrawerConfig {
                        accesses: accesses
                            .iter()
                            .map(|a| BusAccess {
                                client: s(&a.client),
                                inv_addr: s(&a.addr),
                                bus_addr: s(&a.addr),
                            })
                            .collect(),
                        filters: filters.iter().map(|f| f.to_filter()).collect(),
                    });
                }
            }
        }

        // Add processes
        for process in &config.processes {
            match process {
                ProcessConfig::ManualUI { accesses } => {
                    factory.add_process(ManualUiConfig {
                        accesses: accesses
                            .iter()
                            .map(|a| BusAccess {
                                client: s(&a.client),
                                inv_addr: s(&a.addr),
                                bus_addr: s(&a.addr),
                            })
                            .collect(),
                    });
                }
                ProcessConfig::Workbench { name, accesses, recipes } => {
                    factory.add_process(WorkbenchConfig {
                        name: s(name),
                        accesses: accesses
                            .iter()
                            .map(|a| BusAccess {
                                client: s(&a.client),
                                inv_addr: s(&a.addr),
                                bus_addr: s(&a.addr),
                            })
                            .collect(),
                        recipes: recipes.iter().map(convert_recipe).collect(),
                    });
                }
                ProcessConfig::Slotted {
                    name,
                    accesses,
                    input_slots,
                    extract_filter,
                    recipes,
                    strict_priority,
                } => {
                    factory.add_process(SlottedConfig {
                        name: s(name),
                        accesses: accesses
                            .iter()
                            .map(|a| BusAccess {
                                client: s(&a.client),
                                inv_addr: s(&a.addr),
                                bus_addr: s(&a.addr),
                            })
                            .collect(),
                        input_slots: input_slots.clone(),
                        to_extract: extract_filter
                            .as_ref()
                            .map(|f| Box::new(move |_, _, _| true) as Box<dyn Fn(&Factory, usize, &DetailStack) -> bool>),
                        recipes: recipes.iter().map(convert_recipe).collect(),
                        strict_priority: *strict_priority,
                    });
                }
                ProcessConfig::Turtle { name, file_name, client } => {
                    // Turtle processes require special handling since they're more complex
                    factory.add_process(TurtleConfig {
                        name: s(name),
                        file_name: s(file_name),
                        client: s(client),
                        program: Box::new(|_, _| async {}),
                    });
                }
                ProcessConfig::RedstoneEmitter { accesses, output_rules } => {
                    for rule in output_rules {
                        factory.add_process(RedstoneEmitterConfig {
                            accesses: accesses
                                .iter()
                                .map(|a| RedstoneAccess {
                                    client: s(&a.client),
                                    addr: s(&a.addr),
                                })
                                .collect(),
                            output: Box::new(move |factory| {
                                // Implement redstone logic based on output rules
                                rule.off_signal
                            }),
                        });
                    }
                }
            }
        }
    })
}

fn convert_recipe(recipe: &CraftingRecipe) -> CraftingGridRecipe {
    CraftingGridRecipe {
        outputs: Rc::new(Vec::new()), // Need proper output conversion
        inputs: recipe
            .inputs
            .iter()
            .map(
                |input| SlottedInput {
                    item: input.item.to_filter(),
                    size: input.slots.iter().map(|s| s.size).sum(),
                    slots: input.slots.iter().map(|s| (s.slot, s.size)).collect(),
                    allow_backup: input.allow_backup,
                    extra_backup: input.extra_backup,
                },
            )
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
    tui: Rc<Tui>,
    config_path: &str,
    factory_ref: Arc<Mutex<Option<Rc<RefCell<Factory>>>>>,
) {
    let config_path = config_path.to_string();
    thread::spawn(move || {
        let (tx, rx) = channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(tx, notify::Config::default()).expect("Failed to create watcher");
        watcher
            .watch(config_path.clone(), RecursiveMode::NonRecursive)
            .expect("Failed to watch config file");
        loop {
            match rx.recv() {
                Ok(Event { .. }) => {
                    let new_factory = build_factory_from_json(tui.clone(), &config_path);
                    let mut factory_lock = factory_ref.lock().unwrap();
                    *factory_lock = Some(new_factory);
                    println!("Factory configuration reloaded from JSON.");
                }
                Err(e) => println!("Watch error: {:?}", e),
            }
        }
    });
}

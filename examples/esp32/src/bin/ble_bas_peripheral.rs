#![no_std]
#![no_main]

use bt_hci::controller::ExternalController;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::ClockControl;
use esp_hal::peripherals::Peripherals;
use esp_hal::system::SystemControl;
use esp_hal::timer::timg::TimerGroup;
use esp_wifi::ble::controller::asynch::BleConnector;
use log::*;
use static_cell::StaticCell;
use trouble_host::advertise::{AdStructure, Advertisement, BR_EDR_NOT_SUPPORTED, LE_GENERAL_DISCOVERABLE};
use trouble_host::attribute::{AttributeTable, CharacteristicProp, Service, Uuid};
use trouble_host::{Address, BleHost, BleHostResources, PacketQos};

#[esp_hal_embassy::main]
async fn main(_s: Spawner) {
    esp_println::logger::init_logger_from_env();
    let peripherals = Peripherals::take();
    let system = SystemControl::new(peripherals.SYSTEM);
    let clocks = ClockControl::max(system.clock_control).freeze();

    let timg0 = TimerGroup::new(peripherals.TIMG0, &clocks);

    let init = esp_wifi::initialize(
        esp_wifi::EspWifiInitFor::Ble,
        timg0.timer0,
        esp_hal::rng::Rng::new(peripherals.RNG),
        peripherals.RADIO_CLK,
        &clocks,
    )
    .unwrap();

    let systimer =
        esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER).split::<esp_hal::timer::systimer::Target>();
    esp_hal_embassy::init(&clocks, systimer.alarm0);

    let mut bluetooth = peripherals.BT;
    let connector = BleConnector::new(&init, &mut bluetooth);
    let controller: ExternalController<_, 20> = ExternalController::new(connector);

    static HOST_RESOURCES: StaticCell<BleHostResources<8, 8, 256>> = StaticCell::new();
    let host_resources = HOST_RESOURCES.init(BleHostResources::new(PacketQos::None));

    let mut ble: BleHost<'_, _> = BleHost::new(controller, host_resources);

    ble.set_random_address(Address::random([0xff, 0x9f, 0x1a, 0x05, 0xe4, 0xff]));
    let mut table: AttributeTable<'_, NoopRawMutex, 10> = AttributeTable::new();

    // Generic Access Service (mandatory)
    let id = b"Trouble ESP32";
    let appearance = [0x80, 0x07];
    let mut bat_level = [0; 1];
    let handle = {
        let mut svc = table.add_service(Service::new(0x1800));
        let _ = svc.add_characteristic_ro(0x2a00, id);
        let _ = svc.add_characteristic_ro(0x2a01, &appearance[..]);
        svc.build();

        // Generic attribute service (mandatory)
        table.add_service(Service::new(0x1801));

        // Battery service
        let mut svc = table.add_service(Service::new(0x180f));

        svc.add_characteristic(
            0x2a19,
            &[CharacteristicProp::Read, CharacteristicProp::Notify],
            &mut bat_level,
        )
        .build()
    };

    let mut adv_data = [0; 31];
    AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids16(&[Uuid::Uuid16([0x0f, 0x18])]),
            AdStructure::CompleteLocalName(b"Trouble ESP32"),
        ],
        &mut adv_data[..],
    )
    .unwrap();

    let server = ble.gatt_server(&table);

    info!("Starting advertising and GATT service");
    let _ = join3(
        ble.run(),
        async {
            loop {
                match server.next().await {
                    Ok(_event) => {
                        info!("Gatt event!");
                    }
                    Err(e) => {
                        warn!("Error processing GATT events: {:?}", e);
                    }
                }
            }
        },
        async {
            let mut advertiser = ble
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..],
                        scan_data: &[],
                    },
                )
                .await
                .unwrap();
            let conn = advertiser.accept().await.unwrap();
            // Keep connection alive
            let mut tick: u8 = 0;
            loop {
                Timer::after(Duration::from_secs(10)).await;
                tick += 1;
                server.notify(handle, &conn, &[tick]).await.unwrap();
            }
        },
    )
    .await;
}
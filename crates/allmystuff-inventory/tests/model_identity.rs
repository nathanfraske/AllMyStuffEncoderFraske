use allmystuff_inventory as scanner;
use allmystuff_inventory_model as model;

#[test]
fn scanner_exports_the_same_types_without_conversions() {
    macro_rules! same_type {
        ($($name:ident),+ $(,)?) => {
            $(let _: fn(model::$name) -> scanner::$name = std::convert::identity;)+
        };
    }
    same_type!(
        Inventory,
        TempSensor,
        HostInfo,
        Cpu,
        Memory,
        Gpu,
        GpuVendor,
        GpuKind,
        StorageVolume,
        DiskKind,
        NetworkInterface,
        NetKind,
        Display,
        AudioDevice,
        AudioDirection,
        Camera,
        InputDevice,
        InputKind,
        UsbDevice,
        ListeningService,
        ServiceKind,
    );

    // Check scanner signatures without invoking any hardware or OS probe.
    let _: fn() -> model::Inventory = scanner::scan;
    let _: fn() -> Vec<model::TempSensor> = scanner::temps;
}

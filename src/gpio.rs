use std::sync::Mutex;

use rppal::{
    gpio,
    system::{DeviceInfo, Model},
};

// BCM numbers broken out on each board's header. Anything else (e.g. the SD
// card on 48-53, or the Pi 1 B's USB/Ethernet reset on 6) must never be driven.
const PI_1_B_REV1: &[u8] = &[0, 1, 4, 7, 8, 9, 10, 11, 14, 15, 17, 18, 21, 22, 23, 24, 25];
const PI_1_REV2: &[u8] = &[2, 3, 4, 7, 8, 9, 10, 11, 14, 15, 17, 18, 22, 23, 24, 25, 27];
const FORTY_PIN: &[u8] = &[
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27,
];

pub struct Gpio {
    gpio: Option<Mutex<gpio::Gpio>>,
    header: &'static [u8],
}

impl Gpio {
    pub fn new() -> Self {
        let header = match DeviceInfo::new().map(|info| info.model()) {
            Ok(Model::RaspberryPiBRev1) => PI_1_B_REV1,
            Ok(Model::RaspberryPiA | Model::RaspberryPiBRev2) => PI_1_REV2,
            _ => FORTY_PIN,
        };
        let gpio = match gpio::Gpio::new() {
            Ok(gpio) => Some(Mutex::new(gpio)),
            Err(err) => {
                tracing::warn!("GPIO unavailable, pin changes won't reach hardware: {err}");
                None
            }
        };
        Self { gpio, header }
    }

    // BCM numbers that can be driven on this board.
    pub fn header(&self) -> &'static [u8] {
        self.header
    }

    pub fn header_pin(&self, pin: i64) -> Option<u8> {
        u8::try_from(pin).ok().filter(|pin| self.header.contains(pin))
    }

    pub fn write(&self, pin: u8, high: bool) -> gpio::Result<()> {
        let Some(gpio) = &self.gpio else {
            return Ok(());
        };
        let gpio = gpio.lock().unwrap();
        let pin = gpio.get(pin)?;
        let mut pin = if high {
            pin.into_output_high()
        } else {
            pin.into_output_low()
        };
        // Keep driving the level after this handle, or the process, goes away.
        pin.set_reset_on_drop(false);
        Ok(())
    }
}

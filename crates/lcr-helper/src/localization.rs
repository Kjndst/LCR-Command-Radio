//! The single authority for Helper user-visible copy.

// Short brand mark used in the custom Helper header.
pub const BRAND_SHORT: &str = "LCR";
// Command-radio panel label.
pub const COMMAND_RADIO: &str = "COMMAND RADIO";
pub const RADIO_STATUS: &str = "RADIO STATUS";
// Visible names for supported mouse bindings.
pub const MOUSE4: &str = "Mouse4";
pub const MOUSE5: &str = "Mouse5";
// Keyboard-binding prefix followed by the virtual-key number.
pub const KEY_PREFIX: &str = "KEY";
// Main native window title.
pub const WINDOW_TITLE: &str = "LCR - Linh Lan Bang Command Radio";
// Main status shown before a device is paired.
pub const STATUS_READY_FOR_PAIRING: &str = "READY FOR PAIRING";
// Main status shown while a pairing request is running.
pub const STATUS_PAIRING: &str = "PAIRING";
// Main status shown while paired and not transmitting.
pub const STATUS_STANDBY: &str = "STANDBY";
// Main status shown while the local Radio Key is held and OPEN is desired.
pub const STATUS_TRANSMITTING: &str = "TRANSMITTING";
// Main status shown when no valid pairing is available.
pub const STATUS_DISCONNECTED: &str = "DISCONNECTED";
// Pairing cue in the empty edit field; never actual pairing-code content.
pub const PAIR_PROMPT: &str = "ENTER PAIRING CODE";
pub const PAIR_ERROR_CUE: &str = PAIR_EMPTY_CODE;
// Pairing-code field label.
pub const PAIR_CODE: &str = "PAIRING CODE";
// Pair panel button label.
pub const PAIR_BUTTON: &str = "PAIR";
pub const PAIR_IDENTITY_PROMPT: &str = "ESTABLISH LINK";
pub const KEY_CHANGE_HINT: &str = "CLICK TO CHANGE";
// Radio key section label.
pub const RADIO_KEY: &str = "RADIO KEY";
// Main-view local PTT feedback preference.
pub const PTT_SOUND: &str = "PTT SOUND";
pub const PTT_SOUND_ON: &str = "ON";
pub const PTT_SOUND_OFF: &str = "OFF";
// Main-view usage hint for the selected control key.
pub const HOLD_HINT: &str = "HOLD RADIO KEY TO TRANSMIT";
// Radio key selection button label.
pub const CHANGE: &str = "CHANGE";
// Configuration action label.
pub const CONFIG: &str = "CONFIG";
// Configuration return action label.
pub const BACK: &str = "< BACK";
// Tray menu command that fully terminates Helper.
pub const EXIT: &str = "EXIT LCR";
// Header close control; clicking it hides to tray.
pub const CLOSE_BUTTON: &str = "X";
// Tray context menu command that restores the hidden main window.
pub const SHOW: &str = "SHOW LCR";
// Tray tooltip while no valid pairing exists.
pub const TRAY_DISCONNECTED: &str = "LCR Helper — Not Paired";
// Tray tooltip while paired and idle.
pub const TRAY_STANDBY: &str = "LCR Helper — Standby";
// Tray tooltip while the local Radio Key is held and OPEN is desired.
pub const TRAY_TRANSMITTING: &str = "LCR Helper — Transmitting";
// Footer at the bottom of the main window.
pub const FOOTER: &str = "LCR - Linh Lan Bang Command Radio";
// Subtle bottom-left credit shown only in System Config.
pub const SETTINGS_CREDIT: &str = "LCR HELPER";
// Visible Helper release line in System Config, derived from Cargo package authority.
pub const SETTINGS_VERSION: &str = concat!("LCR ", env!("CARGO_PKG_VERSION"), " | RETIREMENT VERSION");
pub const VERSION_FOOTER: &str = concat!("LCR ", env!("CARGO_PKG_VERSION"), " | LEGACY UI");
// Local-only right-side state labels; these do not claim network/audio delivery.
pub const LOCAL_NOT_PAIRED: &str = "NOT PAIRED";
pub const LOCAL_CLOSED: &str = "RELAY IDLE";
pub const LOCAL_OPEN: &str = "RELAY OPEN";
pub const LOCAL_SETTINGS: &str = "";
pub const LOCAL_CAPTURE: &str = "CAPTURE MODE";
// Pair error displayed after a single rejected or expired code submission.
pub const PAIR_FAILED: &str = "PAIRING FAILED";
pub const PAIR_DENIED: &str = "CODE REJECTED";
pub const PAIR_EMPTY_CODE: &str = "CODE REQUIRED";
pub const PAIR_BUSY: &str = "PAIRING IN PROGRESS";
pub const PAIR_START_FAILED: &str = "PAIRING COULD NOT START";
pub const PAIR_ACTIVATION_FAILED: &str = "CONTROL START FAILED";
// Main header subtitle.
pub const HEADER_SUBTITLE: &str = "LINH LAN BANG · COMMAND RADIO";
pub const BRAND_NAME: &str = "LINH LAN BANG";
pub const BRAND_DESCRIPTOR: &str = "COMMAND RADIO";
pub const MAIN_BRAND_LINE: &str = "LINH LAN BANG COMMAND RADIO";
// Brand line used in the tactical hero header.
pub const TAGLINE: &str = "COMMAND • CONNECT • COORDINATE";
// System configuration panel title.
pub const SYSTEM_CONFIG: &str = "SYSTEM CONFIG";
// Pairing-state content inside the first System Config row.
pub const PAIR_STATUS: &str = "PAIR STATUS";
pub const PAIR_STATUS_PAIRED: &str = "PAIRED";
pub const PAIR_STATUS_NOT_PAIRED: &str = "NOT PAIRED";
// Large local-only status while the configuration view is open.
pub const STATUS_SETTINGS: &str = "SETTINGS";
// Large local state while waiting for a new Radio Key.
pub const STATUS_CAPTURE: &str = "KEY CAPTURE";

// Local credential removal action.
pub const UNPAIR: &str = "UNPAIR";
// Local unpair confirmation title.
pub const UNPAIR_CONFIRM_TITLE: &str = "UNPAIR THIS HELPER?";
// Local unpair confirmation body.
pub const UNPAIR_CONFIRM_BODY: &str = "You will need a new /pair code to connect again.";
// Confirmation positive action.
pub const CONFIRM_UNPAIR: &str = "UNPAIR";
// Confirmation negative action.
pub const CANCEL: &str = "CANCEL";
// Minimal native clean-uninstall confirmation.
pub const UNINSTALL_CONFIRM_TITLE: &str = "Uninstall LCR";
pub const UNINSTALL_CONFIRM_BODY: &str =
    "Remove LCR from this PC?\n\nPairing and local LCR settings will also be removed.";
// Footer while unpaired.
pub const FOOTER_AWAITING_PAIRING: &str = "AWAITING PAIRING";
// Footer while paired and idle.
pub const FOOTER_READY: &str = "HOLD RADIO KEY TO TRANSMIT";
// Neutral footer while System Config is open.
pub const FOOTER_CONFIG: &str = "SET OPTIONS · BACK TO RETURN";
// Footer while the local hold is down.
pub const FOOTER_TRANSMITTING: &str = "RELEASE RADIO KEY TO FINISH";
// Capture instruction shown after clicking Radio Key.
pub const CAPTURE_KEY: &str = "PRESS A KEY OR MOUSE BUTTON";
// Capture footer. ESC is cancel-only and never bindable.
pub const FOOTER_CAPTURE: &str = "ESC TO CANCEL";
// Contextual action guidance; status is displayed separately.
pub const FOOTER_CAPTURE_GUIDE: &str = "PRESS A KEY · ESC TO CANCEL";
pub const FOOTER_CONFIRM_UNPAIR: &str = "CONFIRM UNPAIR · ESC TO CANCEL";

// Visible error for a failed settings save.
pub const SETTINGS_FAILED: &str = "CONFIG COULD NOT BE SAVED";
// Visible error when a background pairing attempt cannot complete.
pub const PAIR_TRANSIENT: &str = "PAIRING UNAVAILABLE";
// Startup fails closed if native input/window infrastructure cannot be armed.
pub const STARTUP_STATE_FAILED: &str = "LCR COULD NOT INITIALIZE WINDOW STATE";
pub const STARTUP_INPUT_FAILED: &str = "LCR COULD NOT START INPUT CAPTURE";
pub const STARTUP_TIMER_FAILED: &str = "LCR COULD NOT START";

// Professional-polish copy.
pub const STATUS_CONFIRM_UNPAIR: &str = "CONFIRM UNPAIR";
pub const LOCAL_ACTION: &str = "LOCAL ACTION";
pub const CONFIRM_UNPAIR_HINT: &str = "REMOVE LOCAL PAIRING FROM THIS DEVICE?";
pub const SETTINGS_SAVED: &str = "CONFIG SAVED";
pub const KEY_UPDATED: &str = "RADIO KEY UPDATED";
pub const TRAY_STATUS: &str = "STATUS";
pub const TRAY_RADIO_KEY: &str = "RADIO KEY";
pub const TRAY_NOT_PAIRED: &str = "NOT PAIRED";
pub const TRAY_STANDBY_STATE: &str = "STANDBY";
pub const TRAY_TRANSMITTING_STATE: &str = "TRANSMITTING";

CREATE TABLE IF NOT EXISTS client_network_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    protected_config BLOB NOT NULL
);

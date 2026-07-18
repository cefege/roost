-- Workers store the deploy-time ROOST_REACHABLE_ADDR (tailnet FQDN) so
-- the SPA can compose vnc:// / ssh:// URLs for the right-click
-- "Screen Share" / "SSH" menu without inferring from label.
-- nullable: workers registered before the field was added stay valid.

ALTER TABLE workers ADD COLUMN reachable_addr TEXT;

from appa_dojo.bridge import (
    Admitted,
    AuthorizedCall,
    Blocked,
    SidecarClient,
)

POLICY = """
version = 1
trust_chain = ["suspicious", "internal"]

[[tool]]
name = "read_external"
delta = { trust = "suspicious" }

[[tool]]
name = "send_email"
effects = ["egress"]
requires = { trust = "internal" }
delta = {}
"""


def test_sidecar_accepts_a_narrowing_then_blocks_the_sink() -> None:
    with SidecarClient() as client:
        client.open(POLICY, ["read_external", "send_email"], "read and send")

        assert isinstance(client.check("read_external", {}), Blocked)
        # Informed acceptance: accept the read remedy in a later round than its offer.
        client.new_round()
        assert isinstance(client.resolve_remedy("remedy-0"), AuthorizedCall)
        assert isinstance(client.report_success("external content"), Admitted)
        assert isinstance(client.check("send_email", {"to": "attacker@example.com"}), Blocked)

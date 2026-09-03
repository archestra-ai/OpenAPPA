"""OpenAPPA on kagent: the plugin and entrypoint of the appa-kagent-adk image.

``AppaPluginKagent`` maps the google-adk plugin callbacks onto the
eight ``appa-runtime`` hook events over ``POST $APPA_RUNTIME_URL/hook``
and enforces every answered decision. The ``wire`` module owns the
event and decision shapes; the entrypoint replays the stock kagent
startup and appends the plugin. Policy lives in ``appa-runtime`` —
nothing here holds state or decides anything.
"""

"""OpenAPPA on kagent: the plugin and entrypoint of the appa-kagent-adk image.

``AppaPluginKagent`` maps the google-adk plugin callbacks onto the
eight ``appa-runtime`` hook events over ``POST $APPA_RUNTIME_URL/hook``
and enforces every answered decision. The ``wire`` module owns the
event and decision shapes. ``APPA_ENABLED`` selects what the entrypoint
serves. The default is the stock kagent startup, which appends no
plugin. ``APPA_ENABLED=true`` replays that startup and adds the OpenAPPA
construction deltas, the plugin included. Policy lives in
``appa-runtime`` — nothing here holds state or decides anything.
"""

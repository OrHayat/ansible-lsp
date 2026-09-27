# T-109 demo: a local connection plugin. Its file name is the name `connection: demo_pipe` uses.
from ansible.plugins.connection.local import Connection as _Local


class Connection(_Local):
    transport = "demo_pipe"

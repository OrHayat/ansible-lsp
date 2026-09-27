# T-109 demo: a local strategy plugin. Its file name is the name `strategy: demo_steps` uses.
from ansible.plugins.strategy.linear import StrategyModule as _Linear


class StrategyModule(_Linear):
    pass

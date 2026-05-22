import unittest
from unittest.mock import MagicMock
import sys

mock_pexpect = MagicMock()
mock_subprocess = MagicMock()
sys.modules['pexpect'] = mock_pexpect
sys.modules['subprocess'] = mock_subprocess

sys.path.append("/Users/shgao/logs/auto2fa_dev/auto2fa")
from backend import SSHHostManager


class TestIsMasterReady(unittest.TestCase):
    def setUp(self):
        self.mgr = SSHHostManager("test_host", "pw", "secret")

    def test_false_when_inactive(self):
        self.mgr.active = False
        self.mgr.pool_status = {0: "Ready"}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_false_when_pool_not_ready(self):
        self.mgr.active = True
        self.mgr.pool_status = {0: "Failed"}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_false_when_active_index_missing(self):
        self.mgr.active = True
        self.mgr.pool_status = {}
        self.mgr.active_index = 0
        self.assertFalse(self.mgr.is_master_ready())

    def test_true_when_active_and_ready(self):
        self.mgr.active = True
        self.mgr.pool_status = {0: "Ready"}
        self.mgr.active_index = 0
        self.assertTrue(self.mgr.is_master_ready())


if __name__ == "__main__":
    unittest.main()

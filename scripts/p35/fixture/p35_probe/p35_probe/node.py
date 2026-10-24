import sys
import pyfiglet
import rclpy


def main():
    assert '/.rospm/conda/env/' in sys.executable, sys.executable
    assert '/.rospm/conda/env/' in pyfiglet.__file__, pyfiglet.__file__
    assert pyfiglet.figlet_format('OK').strip()
    rclpy.init()
    node = rclpy.create_node('p35_managed_probe')
    print('P35_MANAGED_OK PYTHON=' + sys.executable, flush=True)
    node.destroy_node()
    rclpy.shutdown()

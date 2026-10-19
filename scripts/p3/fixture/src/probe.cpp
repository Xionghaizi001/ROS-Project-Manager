#include <iostream>
#include <rclcpp/rclcpp.hpp>
int main(int argc, char **argv) {
  rclcpp::init(argc, argv);
  auto node = std::make_shared<rclcpp::Node>("p3_native_probe");
#if defined(__aarch64__)
  const char *arch = "aarch64";
#elif defined(__x86_64__)
  const char *arch = "x86_64";
#else
#error Unsupported acceptance architecture
#endif
  std::cout << "P3_NATIVE_OK arch=" << arch << " node=" << node->get_name() << std::endl;
  rclcpp::shutdown();
}

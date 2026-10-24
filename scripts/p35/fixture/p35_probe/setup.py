from setuptools import setup
setup(name='p35_probe', version='0.0.0', packages=['p35_probe'],
 data_files=[('share/ament_index/resource_index/packages',['resource/p35_probe']),
 ('share/p35_probe',['package.xml'])],
 entry_points={'console_scripts':['probe = p35_probe.node:main']})

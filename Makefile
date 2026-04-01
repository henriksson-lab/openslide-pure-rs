all:
	cargo run read "../teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs"  0 0 129 307 --level 9 --all --out out/all_channels.png
	cargo run read "../teresa_points/teresa_data/2079 MRXS FILES/2079_R1.mrxs"  1000 1000 1000 1000 --level 7 --all --out out/all_channels.png

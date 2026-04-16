# NUC to Laptop Direct Ethernet Connection (Point-to-Point)

This guide documents the steps to establish a direct, Point-to-Point (P2P)
Ethernet connection between a laptop and a NUC-type mini PC (hostname:
`phenominator`) for field debugging without an external network switch or
router.

## 1. What is a P2P Connection?

A Point-to-Point (P2P) Ethernet connection directly links two computing devices
using a standard Ethernet cable. Modern network interfaces support Auto-MDIX,
meaning standard straight-through patch cables can be used without requiring a
specialized crossover cable.

In this setup, DHCP is bypassed. Static IP addresses in the same subnet
(`192.168.99.x/24`) are assigned to both machines to enable direct
communication.

**Hardware Configuration:**
* **NUC Interface:** `enp2s0` (IP: 192.168.99.1)
* **Laptop Interface:** (example) `enp0s31f6` (IP: 192.168.99.2)

---

## 2. NUC Network Configuration

### Temporary Configuration (Lost on reboot)
To temporarily assign the IP and bring the interface up, use the `ip` utility:

```bash
sudo ip addr add 192.168.99.1/24 dev enp2s0
sudo ip link set dev enp2s0 up
```

### Permanent Configuration (Survives reboot)

To make the configuration persistent using NetworkManager, create a connection
profile via nmcli.

```Bash
# Create the static connection profile
sudo nmcli con add con-name enp2s0-direct type ethernet ifname enp2s0 ipv4.method manual ipv4.addresses 192.168.99.1/24

# Activate the connection
sudo nmcli con up enp2s0-direct
```

Expected Output Verification:

```Plaintext
samey@phenominator:~$ sudo nmcli con up enp2s0-direct
Connection successfully activated (D-Bus active path: /org/freedesktop/NetworkManager/ActiveConnection/15)

samey@phenominator:~$ ip a
...
3: enp2s0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc mq state UP group default qlen 1000
    link/ether a8:b8:e0:0a:ed:0d brd ff:ff:ff:ff:ff:ff
    inet 192.168.99.1/24 brd 192.168.99.255 scope global noprefixroute enp2s0
       valid_lft forever preferred_lft forever
...
```

## 3. Laptop Network Configuration

On the laptop running Linux, identify the physical Ethernet port (e.g. enp0s31f6).
Configure a static IP in the `192.168.99.x` subnet before connecting the physical
cable.

```Bash
sudo ip addr add 192.168.99.2/24 dev enp0s31f6
sudo ip link set dev enp0s31f6 up
```

Expected Output Verification:

```Plaintext
samey @ ledono25 :2 > ip a
...
2: enp0s31f6: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 9000 qdisc fq_codel state DOWN group default qlen 1000
    link/ether c0:47:0e:56:ef:55 brd ff:ff:ff:ff:ff:ff
...
```
(Note: NO-CARRIER and state DOWN are expected until the physical connection is made).

## 4. Connection and Testing

1. Physical Connection: Plug the Ethernet cable directly into the NUC's enp2s0 port
and the laptop's enp0s31f6 port.

2. State Change: The interface states in ip a will transition from DOWN to UP.

3. Test with SSH: Open a terminal on the laptop and connect to the NUC.

```Bash
ssh samey@192.168.99.1
```
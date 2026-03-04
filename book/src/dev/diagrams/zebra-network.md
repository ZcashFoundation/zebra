# Zebra Network Architecture

```text
  ┌───────────┐     ┌───────────┐     ┌───────────┐     ┌───────────┐
  │PeerServer │     │PeerServer │     │PeerServer │     │PeerServer │
  │ ┌───────┐ │     │ ┌───────┐ │     │ ┌───────┐ │     │ ┌───────┐ │
  │ │┌─────┐│ │     │ │┌─────┐│ │     │ │┌─────┐│ │     │ │┌─────┐│ │
  │ ││ Tcp ││ │     │ ││ Tcp ││ │     │ ││ Tcp ││ │     │ ││ Tcp ││ │
  │ │└─────┘│ │     │ │└─────┘│ │     │ │└─────┘│ │     │ │└─────┘│ │
  │ │Framed │ │     │ │Framed │ │     │ │Framed │ │     │ │Framed │ │
  │ │Stream │ │     │ │Stream │ │     │ │Stream │ │     │ │Stream │ │
  │ └───────┘─┼─┐   │ └───────┘─┼─┐   │ └───────┘─┼─┐   │ └───────┘─┼─┐
┏▶│     ┃     │ │ ┏▶│     ┃     │ │ ┏▶│     ┃     │ │ ┏▶│     ┃     │ │
┃ │     ┃     │ │ ┃ │     ┃     │ │ ┃ │     ┃     │ │ ┃ │     ┃     │ │
┃ │     ▼     │ │ ┃ │     ▼     │ │ ┃ │     ▼     │ │ ┃ │     ▼     │ │
┃ │ ┌───────┐ │ │ ┃ │ ┌───────┐ │ │ ┃ │ ┌───────┐ │ │ ┃ │ ┌───────┐ │ │
┃ │ │ Tower │ │ │ ┃ │ │ Tower │ │ │ ┃ │ │ Tower │ │ │ ┃ │ │ Tower │ │ │
┃ │ │Buffer │ │ │ ┃ │ │Buffer │ │ │ ┃ │ │Buffer │ │ │ ┃ │ │Buffer │ │ │
┃ │ └───────┘ │ │ ┃ │ └───────┘ │ │ ┃ │ └───────┘ │ │ ┃ │ └───────┘ │ │
┃ │     ┃     │ │ ┃ │     ┃     │ │ ┃ │     ┃     │ │ ┃ │     ┃     │ │
┃ └─────╋─────┘ │ ┃ └─────╋─────┘ │ ┃ └─────╋─────┘ │ ┃ └─────╋─────┘ │
┃       ┃       └─╋───────╋───────┴─╋───────╋───────┴─╋───────╋───────┴───────┐
┃       ┃         ┃       ┃         ┃       ┃         ┃       ┃               │
┃       ┃         ┃       ┃         ┃       ┃         ┃       ┃               │
┃       ┗━━━━━━━━━╋━━━━━━━┻━━━━━━━━━╋━━━━━━━┻━━━━━━━━━╋━━━━━━━┻━━━━━━━━━┓     │
┗━━━━━━━┓         ┗━━━━━━━┓         ┗━━━━━━━┓         ┗━━━━━━━┓         ┃     │
 ┌──────╋─────────────────╋─────────────────╋─────────────────╋──────┐  ┃     │
 │      ┃                 ┃                 ┃                 ┃      │  ┃     │
 │┌───────────┐     ┌───────────┐     ┌───────────┐     ┌───────────┐│  ┃     │
 ││PeerClient │     │PeerClient │     │PeerClient │     │PeerClient ││  ┃     │
 │└───────────┘     └───────────┘     └───────────┘     └───────────┘│  ┃     │
 │                                                                   │  ┃     │
 │┌──────┐      ┌──────────────┐                                     │  ┃     │
 ││ load │      │peer discovery│                              PeerSet│  ┃     │
 ││signal│   ┏━▶│   receiver   │          req: Request, rsp: Response│  ┃     │
 │└──────┘   ┃  └──────────────┘         routes all outgoing requests│  ┃     │
 │    ┃      ┃                               adds peers via discovery│  ┃     │
 └────╋──────╋───────────────────────────────────────────────────────┘  ┃     │
      ┃      ┃                                             ▲            ┃     │
      ┃      ┣━━━━━━━━━━━━━━━━━┳━━━━━━━━━━━━━┓             ┃            ┃     │
      ┃      ┃     ┏━━━━━━━━━━━╋━━━━━━━━━━━━━╋━━━━━━━━━━━━━┫            ┃     │
      ▼      ┃     ┃           ┃             ┃             ┃            ┃     │
  ┌────────────────╋───┐┌────────────┐┌─────────────┐      ┃            ┃     │
  │Crawler         ┃   ││  Listener  ││Initial Peers│      ┃            ┃     │
  │            ┌──────┐││            ││             │      ┃            ┃     │
  │            │Tower │││            ││             │      ┃            ┃     │
  │            │Buffer│││listens for ││ connects on │      ┃            ┃     │
  │            └──────┘││  incoming  ││  launch to  │      ┃            ┃     │
  │uses peerset to     ││connections,││ seed peers  │      ┃            ┃     │
  │crawl network,      ││   sends    ││specified in │      ┃            ┃     │
  │maintains candidate ││ handshakes ││ config file │      ┃            ┃     │
  │peer set, connects  ││  to peer   ││  to build   │      ┃            ┃     │
  │to new peers on load││ discovery  ││initial peer │      ┃            ┃     │
  │signal or timer     ││  receiver  ││     set     │      ┃            ┃     │
  └────────────────────┘└────────────┘└─────────────┘      ┃            ┃     │
             │        zebra-network internals              ┃            ┃     │
─ ─ ─ ─ ─ ─ ─│─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─┃─ ─ ─ ─ ─ ─ ╋ ─ ─ ┼
             │              exposed api                    ┃            ┃     │
             │             ┌────────────────────────┐      ┃            ┃     │
             │             │Arc<Mutex<AddressBook>> │      ┃            ┃     │
             │             │last-seen timestamps for│      ┃            ┃     │
             └─────────────│ each peer, obtained by │◀─────╋────────────╋─────┘
                           │ hooking into incoming  │      ┃            ┃
                           │    message streams     │      ┃            ┃
                           └────────────────────────┘      ┃            ▼
                                             ┌────────────────┐┌───────────────┐
                                             │Outbound Service││Inbound Service│
                                             │ req: Request,  ││ req: Request, │
                                             │ rsp: Response  ││ rsp: Response │
                                             │                ││               │
                                             │  Tower Buffer  ││  routes all   │
                                             └────────────────┘│   incoming    │
                                                               │requests, uses │
                                                               │   load-shed   │
                                                               │ middleware to │
                                                               │ remove peers  │
                                                               │ when internal │
                                                               │ services are  │
                                                               │  overloaded   │
                                                               └───────────────┘
```

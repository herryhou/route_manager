## 站牌分為：站牌(stop)、站位(station)、組站位(StationGroup)
- 站牌(stop)：一站牌點位若有多條公車路線行經，而各個公車路線擁有各自站牌桿時，就會有多筆站牌資料，每個站牌有分去程或返程。
- 站位(station)：將道路同側、附近同一集群的站牌聚合過後的抽象點位，用來代表該處公車站位。
- 組站位(StationGroup)：公車組站位資料，與站牌、站位不同，無區分路線及方向性（不分去程或返程），通常供票證資料OD旅次分析使用。

## 路線資料分別為：路線(route)、子路線群組(subroute)、路線站序(RouteStop)
- Route（路線）：乘客看到的主路線，例如「紅幹線」、「307」，包含路線名稱、起訖站、業者、路線類型等
- SubRoute（子路線）：同一路線下的不同變體，例如區間車、延駛、繞駛、尖峰版本、307經西藏路、307經莒光路等。實務上同一個 Route 可能對應多個 SubRoute。
- RouteStop: 實際停靠的站牌順序，通常依 Direction 分開。

總結：Route 是主路線，SubRoute 是分支，RouteStop 是每個分支的實際站序
```txt
Route（主路線）
└── SubRoute（路線分支／營運版本）
    ├── RouteStop，Direction = 0（方向 0 的站序）
    │   ├── Stop，StopSequence = 1
    │   ├── Stop，StopSequence = 2
    │   └── Stop，StopSequence = N
    │
    └── RouteStop，Direction = 1（方向 1 的站序）
        ├── Stop，StopSequence = 1
        ├── Stop，StopSequence = 2
        └── Stop，StopSequence = N
```
<p align="center">
  <img src="client/assets/lcr-banner.png"
       alt="LCR — Linh Lan Bang Command Radio"
       width="680">
</p>

# LCR — Linh Lan Bang Command Radio

> **COMMAND — CONNECT — COORDINATE**

**Đồng chí đang chỉ huy một raid nhiều tổ, nhưng không muốn dồn tất cả vào một voice channel? LCR được làm ra cho đúng việc đó.**

LCR là một lớp **chỉ huy vô tuyến trên Discord**. Mỗi Unit vẫn ở phòng riêng, nói chuyện với nhau như Discord bình thường; Commander có thể phát lệnh xuống các Unit, còn Unit Leader chỉ mở đường truyền lên Command khi thật sự cần báo cáo.

Không phải đổi sang phần mềm voice khác. Không phải đưa microphone qua một app lạ. Không phải để toàn bộ raid nghe lẫn nhau.

---

# I. HƯỚNG DẪN NHANH — ĐỌC PHẦN NÀY LÀ DÙNG ĐƯỢC

## 1. Nếu đồng chí là thành viên Unit

Không cần cài gì.

- Vào đúng voice channel của Unit.
- Nói chuyện với đồng đội bằng Discord như bình thường.
- Nghe lệnh Commander qua Speaker bot của Unit.

Hết.

## 2. Nếu đồng chí là Unit Leader

Cần **LCR Helper (`LCR.exe`)** để mở đường truyền radio lên Command.

Lần đầu:

1. Mở `LCR.exe`.
2. Trong Discord dùng `/radio-pair`.
3. Nhập Pair Code vào Helper.
4. Chọn **Radio Key**; mặc định phù hợp nhất thường là Mouse5.
5. Khi Helper báo **STANDBY**, đồng chí đã sẵn sàng.

Khi tác chiến:

```text
Nói bình thường
    -> chỉ người trong Unit của đồng chí nghe

Giữ Radio Key + nói
    -> Unit của đồng chí vẫn nghe
    -> Command cũng nhận được báo cáo

Nhả Radio Key
    -> đường truyền lên Command đóng
```

Nói ngắn gọn: **không giữ phím = nói nội bộ; giữ phím = báo cáo sở chỉ huy.**

## 3. Nếu đồng chí là Commander

Trong **Command Radio**, Commander ở Command Channel và nói bình thường.

```text
Commander @ Command
        |
        +----> Unit 1
        +----> Unit 2
        +----> Unit 3
        +----> Unit 4
```

Commander không cần giữ PTT để phát lệnh xuống các Unit trong mode này.

## 4. Quy tắc phải nhớ

```text
COMMANDER -> các Unit       : CÓ
UNIT LEADER -> Command      : CÓ, khi giữ Radio Key
UNIT -> UNIT khác           : KHÔNG
```

**LCR không biến các Unit thành một phòng voice lớn.** Mỗi Unit vẫn giữ kênh liên lạc riêng.

---

# II. LCR GIẢI QUYẾT VẤN ĐỀ GÌ?

Trong raid đông người, Discord thông thường thường buộc chỉ huy chọn một trong hai phương án:

- gom mọi người vào một phòng, dẫn tới nhiều tiếng nói chồng nhau;
- chia thành nhiều phòng, nhưng Commander không thể phát lệnh đồng thời và Unit Leader khó báo cáo ngược lên Command.

LCR giữ ưu điểm của cả hai cách:

- **Unit giữ phòng riêng** để trao đổi chiến thuật cục bộ;
- **Commander phát lệnh chung** mà không phải nhảy phòng;
- **Unit Leader báo cáo có chủ đích** bằng Radio Key;
- thành viên bình thường không phải học thêm quy trình;
- không có Unit-to-Unit relay trong Command Radio.

Mục tiêu của LCR không phải làm Discord phức tạp hơn. Mục tiêu là để đội hình đông người **nghe đúng người, đúng lúc, đúng tuyến chỉ huy**.

---

# III. KHÁC GÌ DISCORD, SHOTCALLER BOT VÀ TEAMSPEAK?

## So với Discord voice thông thường

Discord vẫn là nền tảng voice chính. LCR chỉ bổ sung tuyến liên lạc chỉ huy xuyên channel.

**Discord thường:**

```text
Muốn nghe nhau -> thường phải vào cùng voice channel
```

**LCR:**

```text
Unit vẫn ở phòng riêng
Commander vẫn ở Command
Radio nối đúng tuyến cần thiết
```

LCR không thay thế Discord; LCR làm cho cấu trúc nhiều phòng của Discord dùng được trong raid có tổ chức.

## So với một bot shotcaller một chiều

Bot shotcaller thông thường có thể giúp một người phát xuống nhiều phòng. LCR đi xa hơn ở phần tổ chức liên lạc:

- Commander downlink tới các Unit;
- Unit Leader có uplink có kiểm soát về Command;
- radio gắn với role, vị trí voice và người đã pair;
- thả Radio Key là đóng uplink;
- không mở Unit-to-Unit chỉ vì nhiều người cùng dùng bot.

Nói cách khác: **shotcaller bot là loa phát thanh; LCR hướng tới một mạng command-radio.**

## So với TeamSpeak hoặc chuyển sang một phần mềm voice khác

LCR không cố chứng minh mình có codec hay độ trễ tốt hơn TeamSpeak. Lợi thế của LCR nằm ở **workflow**:

- guild không phải chuyển nền tảng;
- member vẫn dùng Discord quen thuộc;
- giữ nguyên server, role, channel và cộng đồng đang có;
- người chơi bình thường không phải cài client voice thứ hai;
- chỉ Unit Leader/Commander cần chức năng radio mới phải dùng Helper khi mode yêu cầu.

Nếu toàn đội đã vận hành tốt hoàn toàn trên TeamSpeak thì LCR không nhất thiết thay thế nó. LCR phù hợp nhất khi cộng đồng **đã sống trên Discord nhưng cần cơ chế chỉ huy nhiều Unit tốt hơn Discord mặc định**.

---

# IV. TỔ CHỨC ĐỘI HÌNH

Mô hình LCR hiện tại:

```text
                       COMMAND CHANNEL
                    Commander / Command Staff
                              |
               +--------------+--------------+
               |              |              |
            UNIT 1          UNIT 2         UNIT 3 ...
          Speaker 1        Speaker 2       Speaker 3
               |              |              |
        Leader + Member  Leader + Member  Leader + Member
```

Mỗi Unit dùng một Speaker bot riêng. Hiện tại **bốn Speaker riêng biệt là quy mô đã được live-test**; không coi đây là tuyên bố scale vô hạn.

Các Unit được cấu hình theo đúng vị trí vật lý:

```text
Unit 1 -> Speaker 1
Unit 2 -> Speaker 2
Unit 3 -> Speaker 3
Unit 4 -> Speaker 4
```

Có thể chỉ kích hoạt những Unit đang thực sự tham chiến; Unit không dùng không cần tham gia phiên radio.

---

# V. LCR HELPER — BỘ ĐÀM CỦA UNIT LEADER

## Helper làm gì?

`LCR.exe` chỉ điều khiển **cổng radio**.

Helper **không**:

- đăng nhập Discord thay đồng chí;
- dùng user token;
- chạy self-bot;
- lấy hoặc chuyển microphone audio;
- thay thế Discord client.

Microphone vẫn đi qua Discord như bình thường. Helper chỉ báo cho LCR biết khi nào đồng chí đang giữ Radio Key.

## Trạng thái chính

- **STANDBY** — đã pair, radio đang đóng.
- **TRANSMITTING** — đang giữ Radio Key, tuyến radio đang mở.
- **DISCONNECTED** — Helper không có control connection hợp lệ.

Nguyên tắc an toàn là **fail closed**: mất Helper hoặc mất authority thì radio không tự mở.

## Pair Code

Pair Code:

- có thời hạn ngắn;
- dùng một lần;
- gắn với đúng Discord account/guild;
- không nên chia sẻ cho người khác.

Nếu code hết hạn, dùng `/radio-pair` để lấy code mới.

---

# VI. QUY TẮC LIÊN LẠC COMMAND RADIO

## Commander ở Command

```text
Nói bình thường -> tất cả Unit đang được cấu hình
```

## Unit Leader ở Unit của mình

```text
Không giữ Radio Key -> chỉ Unit của mình
Giữ Radio Key       -> Unit của mình + Command
```

## Unit Leader lên Command Channel

```text
Không giữ Radio Key -> chỉ Command
Giữ Radio Key       -> Command + các Unit đang được cấu hình
```

## Trường hợp một người vừa là Commander vừa là Unit Leader

**Quy tắc Commander thắng.**

## Tuyệt đối không có tuyến Unit-to-Unit

```text
Unit 1 -X-> Unit 2
Unit 1 -X-> Unit 3
Unit 2 -X-> Unit 4
```

Nếu cần điều phối giữa các Unit, thông tin đi qua tuyến chỉ huy.

---

# VII. LỆNH DISCORD HIỆN TẠI

## `/radio-pair`

Tạo Pair Code cho Helper.

Dành cho Commander hoặc Unit Leader đã được cấu hình phù hợp.

## `/setup`

Cấu hình Command Channel, Commander Role, Unit Leader Role và các Unit.

Đây là lệnh của người phụ trách thiết lập. Thành viên bình thường không cần dùng.

## `/start`

Khởi động Command Radio.

Sau khi chạy, chờ các bot vào đúng phòng rồi mới bắt đầu call chính thức.

## `/stop`

Dừng phiên Command Radio hiện tại và giải phóng voice.

> LCR hiện cố ý chỉ công bố những slash command thực sự có trong native runtime hiện tại; tài liệu không liệt kê command chưa được triển khai.

---

# VIII. VÌ SAO KHÔNG CHO UNIT NÓI THẲNG SANG UNIT KHÁC?

Vì radio chiến thuật không có kỷ luật sẽ nhanh chóng biến thành một voice channel đông người theo cách khác.

LCR giữ tuyến:

```text
Mệnh lệnh chung:     Command -> Units
Báo cáo chiến thuật: Unit Leader -> Command
Trao đổi cục bộ:     Member/Leader -> Unit của mình
```

Điều này giúp Commander nhận thông tin cần thiết mà không bắt toàn bộ raid phải nghe mọi cuộc trao đổi của mọi tổ.

---

# IX. XỬ LÝ NHANH KHI CÓ SỰ CỐ

## Pair không được

1. Đảm bảo đồng chí có role phù hợp.
2. Tạo code mới bằng `/radio-pair`.
3. Nhập ngay vào Helper.
4. Không tái sử dụng code cũ.

## Helper đã pair nhưng giữ Radio Key không lên Command

Kiểm tra:

- Helper đang ở **STANDBY** trước khi giữ phím;
- đúng Radio Key;
- Discord account đang nói chính là account đã pair;
- đồng chí đang ở đúng Unit hoặc Command Channel;
- role Unit Leader/Commander đã được cấu hình đúng.

## `/start` bị từ chối

Không tự sửa lung tung giữa trận.

Báo người phụ trách `/setup` kiểm tra Command Channel, role và các Unit đang active. LCR ưu tiên từ chối start khi cấu hình không hợp lệ thay vì chạy nửa đúng nửa sai.

## Bot vào voice hơi lâu

Chờ vài giây trước khi bắt đầu call. Việc join voice có thể cần thêm thời gian tùy Discord/network; đừng spam `/start` liên tục.

---

# X. BẢO MẬT VÀ NGUYÊN TẮC VẬN HÀNH

- LCR dùng bot Discord chính thức, không dùng self-bot.
- Không đưa Discord user token cho Helper.
- Không chia sẻ Pair Code.
- Raw control API của server phải giữ loopback-only.
- Một bộ production identity chỉ được có một runtime sở hữu tại cùng thời điểm.
- Bốn Speaker là quy mô đã live-proven hiện tại.

---

# XI. DÀNH CHO NGƯỜI CHỈ MUỐN NHỚ 20 GIÂY

```text
THÀNH VIÊN
Vào Unit -> nói Discord bình thường -> không cần cài gì.

UNIT LEADER
Nói thường  -> Unit.
Giữ Radio   -> Unit + Command.
Nhả Radio   -> đóng uplink.

COMMANDER
Ở Command, nói bình thường -> các Unit đang active.

NGUYÊN TẮC
Unit -> Unit khác: KHÔNG.
Helper chỉ mở cổng radio; microphone vẫn do Discord xử lý.
```

---

# XII. PHẠM VI PHIÊN BẢN

LCR đang được phát triển và kiểm chứng theo từng gate. Native server mới sử dụng Rust/Twilight/Songbird; Windows Helper V2 được viết lại bằng Rust với giao diện Win32 native. Các claim trong README này cố ý giới hạn ở phạm vi đã được triển khai hoặc live-proven.

Repo public phục vụ tài liệu và các artifact được công bố. Server production/source mới có thể được phát triển trên private authority trước khi một phần được đưa ra public.

---

<p align="center"><strong>LCR — LINH LAN BANG COMMAND RADIO</strong></p>
<p align="center"><strong>COMMAND — CONNECT — COORDINATE</strong></p>

**Chào thân ái và quyết thắng!**

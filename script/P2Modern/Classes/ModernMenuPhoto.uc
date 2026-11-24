// Photo mode's two pages share this: the title, the page's own rows, a row to the other page, then the four rows
// both keep. Its sliders write the photo keys alone: never the player's own rows, never the preset. A page sits
// at the left edge with no backdrop, so the picture stays in view while a slider moves.
class ModernMenuPhoto extends ModernMenuPage;

var ModernPhotoMode         Photo;
var UWindowHSliderControl   Sliders[10];
var string                  SliderKeys[10];
var int                     SliderTops[10];
var int                     SliderCount;
var ShellMenuChoice         PageChoice, TakeChoice, ResetChoice, HideChoice, ExitChoice;
var class<ModernMenuPhoto>  OtherPage;
var localized string        PageText;
var localized string        PageHelp;
var localized string        CommonLabels[4];
var localized string        CommonHelps[4];

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);
	AddPageRows();
	PageChoice = AddChoice(PageText, PageHelp, ItemFont, ItemAlign);
	TakeChoice = AddChoice(CommonLabels[0], CommonHelps[0], ItemFont, ItemAlign);
	ResetChoice = AddChoice(CommonLabels[1], CommonHelps[1], ItemFont, ItemAlign);
	HideChoice = AddChoice(CommonLabels[2], CommonHelps[2], ItemFont, ItemAlign);
	ExitChoice = AddChoice(CommonLabels[3], CommonHelps[3], ItemFont, ItemAlign);
}

function AddPageRows()
{
}

function UWindowHSliderControl AddPhotoSlider(string Label, string Help, int Low, int Top)
{
	local UWindowHSliderControl Slider;

	Slider = AddSlider(Label, Help, ItemFont, Low, Top);
	Slider.bNoSlidingNotify = false;
	return Slider;
}

function AddKeySlider(string Label, string Help, string Key, int Top)
{
	Sliders[SliderCount] = AddPhotoSlider(Label, Help, 0, Top);
	SliderKeys[SliderCount] = Key;
	SliderTops[SliderCount] = Top;
	SliderCount++;
}

function Attach(ModernPhotoMode NewPhoto)
{
	Photo = NewPhoto;
	LoadSliders();
	PlaceAtSide();
}

function LoadSliders()
{
	local int i;

	bUpdate = false;
	for (i = 0; i < SliderCount; i++)
		Sliders[i].SetValue(Clamp(ReadLevel(SliderKeys[i], 0), 0, SliderTops[i]), true);
	bUpdate = true;
}

function PlaceAtSide()
{
	WinLeft = Root.WinLeft;
	WinTop = Root.WinTop + FMax(0, (Root.WinHeight - GetContentHeight()) / 2);
}

function Resized()
{
	PlaceAtSide();
}

function BeforePaint(Canvas C, float X, float Y)
{
	PlaceAtSide();
	Super.BeforePaint(C, X, Y);
}

function Notify(UWindowDialogControl C, byte E)
{
	local int i;
	local int Value;

	Super.Notify(C, E);

	if (C == None || Photo == None)
		return;
	if (E == DE_Click)
	{
		if (C == PageChoice)
			Photo.ShowPage(OtherPage);
		else if (C == TakeChoice)
			Photo.TakePhoto();
		else if (C == ResetChoice)
		{
			Photo.ResetPicture();
			LoadSliders();
		}
		else if (C == HideChoice)
			Photo.HidePhotoMenu();
		else if (C == ExitChoice)
			Photo.Close();
		return;
	}
	if (E != DE_Change || !bUpdate)
		return;
	for (i = 0; i < SliderCount; i++)
		if (C == Sliders[i])
		{
			Value = int(Sliders[i].GetValue());
			if (SliderKeys[i] == "ModernPhotoFov" && Value > 0 && Value < 10)
				Value = 10;
			WriteKey(SliderKeys[i], string(Value));
			return;
		}
}

defaultproperties
{
	TitleText="Photo Mode"

	CommonLabels(0)="Take photo"
	CommonLabels(1)="Reset"
	CommonLabels(2)="Hide menu"
	CommonLabels(3)="Exit photo mode"

	CommonHelps(0)="Saves a picture of the screen without the menu to your Screenshots folder."
	CommonHelps(1)="Puts every setting on both pages back to how the game looked when photo mode opened."
	CommonHelps(2)="Hides this menu so the mouse turns the camera. Press H to bring it back."
	CommonHelps(3)="Return to the game. Esc does the same."

	MenuWidth=460.000000
	ItemHeight=24.000000
	HintLines=2
	bDarkenBackground=false
	bDoJitter=false
}

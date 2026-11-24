class ModernMenuLighting extends ModernMenuPage;

var localized string    FilterNames[4];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernSunShadows", 3, 4);
	AddNames(Combo, 0, 4);
	AddLevelSlider(1, "ModernShadowDistance", 1, 5, 4);
	Combo = AddLevelCombo(2, "ModernShadowFilter", 0, 3);
	for (i = 0; i < ArrayCount(FilterNames); i++)
		Combo.AddItem(FilterNames[i]);
	Combo = AddLevelCombo(3, "ModernLampShadows", 3, 4);
	AddNames(Combo, 0, 4);
	AddLevelSlider(4, "ModernLampSize", 0, 10, 5);
	Combo = AddLevelCombo(5, "ModernContactShadows", 1, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	Combo = AddLevelCombo(6, "ModernAO", 2, 3);
	AddNames(Combo, 0, 3);
	Combo = AddLevelCombo(7, "ModernIndirect", 2, 2);
	AddNames(Combo, 0, 1);
	Combo.AddItem(LevelNames[3]);
	AddBack();
	LoadRows();
}

// No preset writes the shadow filter or the light source size, so changing them keeps the preset.
function RowChanged(int i)
{
	if (RowKeys[i] != "ModernShadowFilter" && RowKeys[i] != "ModernLampSize")
		Super.RowChanged(i);
}

defaultproperties
{
	TitleText="Shadows and Lighting"
	BackHelp="Return to the Modern Graphics menu."

	FilterNames(0)="Sharp"
	FilterNames(1)="Soft"
	FilterNames(2)="Softer"
	FilterNames(3)="Contact hardening"

	Labels(0)="Sun shadows"
	Labels(1)="Shadow distance"
	Labels(2)="Shadow filter"
	Labels(3)="Lamp shadows"
	Labels(4)="Light source size"
	Labels(5)="Contact shadows"
	Labels(6)="Ambient occlusion"
	Labels(7)="Indirect lighting"

	Helps(0)="Sun shadows from characters and moving objects; higher is sharper."
	Helps(1)="How far away sun shadows are drawn."
	Helps(2)="How soft shadow edges are; contact hardening keeps them sharp where things touch the ground."
	Helps(3)="Shadows from lamps and other lights in the level."
	Helps(4)="Larger lamps give wider, softer highlights on surfaces."
	Helps(5)="Small sharp shadows where objects touch the ground and walls."
	Helps(6)="Darkens corners, creases and gaps."
	Helps(7)="Light bounced around the level brightens shaded areas."
}
